use crate::base::{Clip, ClipMatrixHandler, MatrixSettings, RelevantContent, Slot};
use crate::rt::supplier::{ChainEquipment, RecorderRequest};
use crate::rt::{
    ClipChangeEvent, ColumnCommandSender, ColumnEvent, ColumnFillSlotArgs, ColumnPlayRowArgs,
    ColumnPlaySlotArgs, ColumnStopArgs, ColumnStopSlotArgs, FillClipMode,
    OverridableMatrixSettings, SharedColumn, SlotChangeEvent, WeakColumn,
};
use crate::{rt, source_util, ClipEngineResult};
use crossbeam_channel::{Receiver, Sender};
use either::Either;
use enumflags2::BitFlags;
use helgoboss_learn::UnitValue;
use playtime_api::persistence as api;
use playtime_api::persistence::{
    preferred_clip_midi_settings, BeatTimeBase, ClipAudioSettings, ClipColor, ClipTimeBase,
    ColumnClipPlayAudioSettings, ColumnClipPlaySettings, ColumnClipRecordSettings, ColumnPlayMode,
    Db, MatrixClipRecordSettings, PositiveBeat, PositiveSecond, Section, TimeSignature,
};
use reaper_high::{Guid, OrCurrentProject, Project, Reaper, Track};
use reaper_low::raw::preview_register_t;
use reaper_medium::{
    create_custom_owned_pcm_source, Bpm, CustomPcmSource, FlexibleOwnedPcmSource, HelpMode,
    MeasureAlignment, OwnedPreviewRegister, ReaperMutex, ReaperVolumeValue,
};
use std::iter;
use std::ptr::NonNull;
use std::sync::Arc;

pub type SharedRegister = Arc<ReaperMutex<OwnedPreviewRegister>>;

#[derive(Clone, Debug)]
pub struct Column {
    settings: ColumnSettings,
    rt_settings: rt::ColumnSettings,
    rt_command_sender: ColumnCommandSender,
    rt_column: SharedColumn,
    preview_register: Option<PlayingPreviewRegister>,
    slots: Vec<Slot>,
    event_receiver: Receiver<ColumnEvent>,
    project: Option<Project>,
}

#[derive(Clone, Debug, Default)]
pub struct ColumnSettings {
    pub clip_record_settings: ColumnClipRecordSettings,
}

#[derive(Clone, Debug)]
struct PlayingPreviewRegister {
    _preview_register: SharedRegister,
    play_handle: NonNull<preview_register_t>,
    track: Option<Track>,
}

impl Column {
    pub fn new(permanent_project: Option<Project>) -> Self {
        let (command_sender, command_receiver) = crossbeam_channel::bounded(500);
        let (event_sender, event_receiver) = crossbeam_channel::bounded(500);
        let source = rt::Column::new(permanent_project, command_receiver, event_sender);
        let shared_source = SharedColumn::new(source);
        Self {
            settings: Default::default(),
            rt_settings: Default::default(),
            // preview_register: {
            //     PlayingPreviewRegister::new(shared_source.clone(), track.as_ref())
            // },
            preview_register: None,
            rt_column: shared_source,
            rt_command_sender: ColumnCommandSender::new(command_sender),
            slots: vec![],
            event_receiver,
            project: permanent_project,
        }
    }

    pub fn set_play_mode(&mut self, play_mode: ColumnPlayMode) {
        self.rt_settings.play_mode = play_mode;
    }

    pub fn duplicate_without_contents(&self) -> Self {
        let mut duplicate = Self::new(self.project);
        duplicate.settings = self.settings.clone();
        duplicate.rt_settings = self.rt_settings.clone();
        if let Some(pr) = &self.preview_register {
            duplicate.init_preview_register(pr.track.clone());
        }
        duplicate
    }

    /// Returns the sender for sending commands to the corresponding real-time column.
    pub fn rt_command_sender(&self) -> &ColumnCommandSender {
        &self.rt_command_sender
    }

    pub fn load(
        &mut self,
        api_column: api::Column,
        chain_equipment: &ChainEquipment,
        recorder_request_sender: &Sender<RecorderRequest>,
        matrix_settings: &MatrixSettings,
    ) -> ClipEngineResult<()> {
        self.clear_slots();
        // Track
        let track = if let Some(id) = api_column.clip_play_settings.track.as_ref() {
            let guid = Guid::from_string_without_braces(id.get())?;
            self.project.or_current_project().track_by_guid(&guid).ok()
        } else {
            None
        };
        self.init_preview_register(track);
        // Settings
        self.settings.clip_record_settings = api_column.clip_record_settings;
        self.rt_settings.audio_resample_mode =
            api_column.clip_play_settings.audio_settings.resample_mode;
        self.rt_settings.audio_time_stretch_mode = api_column
            .clip_play_settings
            .audio_settings
            .time_stretch_mode;
        self.rt_settings.audio_cache_behavior =
            api_column.clip_play_settings.audio_settings.cache_behavior;
        self.rt_settings.play_mode = api_column.clip_play_settings.mode.unwrap_or_default();
        self.rt_settings.clip_play_start_timing = api_column.clip_play_settings.start_timing;
        self.rt_settings.clip_play_stop_timing = api_column.clip_play_settings.stop_timing;
        // Slots
        for api_slot in api_column.slots.unwrap_or_default() {
            let row = api_slot.row;
            for api_clip in api_slot.into_clips() {
                let clip = Clip::load(api_clip);
                let slot = get_slot_mut_insert(&mut self.slots, row);
                fill_slot_with_clip_internal(
                    slot,
                    clip,
                    chain_equipment,
                    recorder_request_sender,
                    matrix_settings,
                    &self.rt_settings,
                    &self.rt_command_sender,
                    self.project,
                    FillClipMode::Add,
                )?;
            }
        }
        Ok(())
    }

    fn init_preview_register(&mut self, track: Option<Track>) {
        self.preview_register = Some(PlayingPreviewRegister::new(self.rt_column.clone(), track));
    }

    pub fn sync_settings_to_rt(&self, matrix_settings: &MatrixSettings) {
        self.rt_command_sender
            .update_settings(self.rt_settings.clone());
        self.rt_command_sender
            .update_matrix_settings(matrix_settings.overridable.clone());
    }

    /// Returns all clips that are currently playing (along with slot index) .
    pub(crate) fn playing_clips(&self) -> impl Iterator<Item = (usize, &Clip)> + '_ {
        // TODO-high This is used for building a scene from the currently playing clips.
        //  If multiple clips are currently playing in one column, we shouldn't add new columns
        //  but put the clips into one slot! This is a new possibility and this is a good use case!
        self.slots.iter().enumerate().flat_map(|(i, s)| {
            let is_playing = s
                .play_state()
                .map(|ps| ps.is_as_good_as_playing())
                .unwrap_or(false);
            if is_playing {
                Either::Left(s.clips().map(move |c| (i, c)))
            } else {
                Either::Right(iter::empty())
            }
        })
    }

    pub fn clear_slots(&mut self) {
        self.slots.clear();
        self.rt_command_sender.clear_slots();
    }

    pub fn find_slot(&self, index: usize) -> Option<&Slot> {
        self.slots.get(index)
    }

    pub fn get_slot(&self, index: usize) -> ClipEngineResult<&Slot> {
        self.slots.get(index).ok_or(SLOT_DOESNT_EXIST)
    }

    pub(crate) fn get_slot_mut(&mut self, index: usize) -> ClipEngineResult<&mut Slot> {
        get_slot_mut(&mut self.slots, index)
    }

    pub(crate) fn get_slot_kit_mut(&mut self, index: usize) -> ClipEngineResult<SlotKit> {
        let kit = SlotKit {
            sender: &self.rt_command_sender,
            slot: get_slot_mut(&mut self.slots, index)?,
        };
        Ok(kit)
    }

    /// Returns whether the slot at the given index is empty.
    pub(crate) fn slot_is_empty(&self, index: usize) -> bool {
        match self.slots.get(index) {
            None => true,
            Some(s) => s.is_empty(),
        }
    }

    /// Returns the actual number of slots in this column.
    ///
    /// Just interesting for internal usage. For external usage, the matrix row count is important.
    pub(super) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub fn save(&self) -> api::Column {
        let track_id = self.preview_register.as_ref().and_then(|reg| {
            reg.track
                .as_ref()
                .map(|t| t.guid().to_string_without_braces())
                .map(api::TrackId::new)
        });
        api::Column {
            clip_play_settings: ColumnClipPlaySettings {
                mode: Some(self.rt_settings.play_mode),
                track: track_id,
                start_timing: self.rt_settings.clip_play_start_timing,
                stop_timing: self.rt_settings.clip_play_stop_timing,
                audio_settings: ColumnClipPlayAudioSettings {
                    resample_mode: self.rt_settings.audio_resample_mode,
                    time_stretch_mode: self.rt_settings.audio_time_stretch_mode,
                    cache_behavior: self.rt_settings.audio_cache_behavior,
                },
            },
            clip_record_settings: self.settings.clip_record_settings.clone(),
            slots: {
                let slots = self
                    .slots
                    .iter()
                    .filter_map(|slot| slot.save(self.project))
                    .collect();
                Some(slots)
            },
        }
    }

    pub fn rt_column(&self) -> WeakColumn {
        self.rt_column.downgrade()
    }

    pub fn poll(&mut self, timeline_tempo: Bpm) -> Vec<(usize, SlotChangeEvent)> {
        // Process source events and generate clip change events
        let mut change_events = vec![];
        while let Ok(evt) = self.event_receiver.try_recv() {
            use ColumnEvent::*;
            let change_event = match evt {
                SlotPlayStateChanged {
                    slot_index,
                    play_state,
                } => {
                    if let Some(slot) = self.slots.get_mut(slot_index) {
                        slot.update_play_state(play_state);
                    }
                    Some((slot_index, SlotChangeEvent::PlayState(play_state)))
                }
                ClipMaterialInfoChanged {
                    slot_index,
                    clip_index,
                    material_info,
                } => {
                    if let Some(slot) = self.slots.get_mut(slot_index) {
                        let _ = slot.update_material_info(clip_index, material_info);
                    }
                    None
                }
                Dispose(_) => None,
                RecordRequestAcknowledged {
                    slot_index, result, ..
                } => {
                    if let Some(slot) = self.slots.get_mut(slot_index) {
                        slot.notify_recording_request_acknowledged(result).unwrap();
                    }
                    None
                }
                MidiOverdubFinished {
                    slot_index,
                    mirror_source,
                } => {
                    if let Some(slot) = self.slots.get_mut(slot_index) {
                        let event = slot
                            .notify_midi_overdub_finished(mirror_source, self.project)
                            .unwrap();
                        Some((slot_index, event))
                    } else {
                        None
                    }
                }
                NormalRecordingFinished {
                    slot_index,
                    outcome,
                } => {
                    let recording_track = &self.effective_recording_track().unwrap();
                    if let Some(slot) = self.slots.get_mut(slot_index) {
                        let event = slot
                            .notify_normal_recording_finished(
                                outcome,
                                self.project,
                                recording_track,
                            )
                            .unwrap();
                        Some((slot_index, event))
                    } else {
                        None
                    }
                }
                InteractionFailed(failure) => {
                    let formatted = format!("Playtime: Interaction failed ({})", failure.message);
                    Reaper::get()
                        .medium_reaper()
                        .help_set(formatted, HelpMode::Temporary);
                    None
                }
                SlotCleared { slot_index, .. } => {
                    if let Some(slot) = self.slots.get_mut(slot_index) {
                        slot.slot_cleared().map(|e| (slot_index, e))
                    } else {
                        None
                    }
                }
            };
            if let Some(evt) = change_event {
                change_events.push(evt);
            }
        }
        // Add position updates
        let continuous_clip_events = self.slots.iter().enumerate().flat_map(|(row, slot)| {
            let nothing = Either::Right(iter::empty());
            let Ok(play_state) = slot.play_state() else {
                return nothing;
            };
            if !play_state.is_advancing() {
                return nothing;
            }
            let temp_project = self.project.or_current_project();
            let iter = match slot.relevant_contents() {
                RelevantContent::Normal(contents) => {
                    let iter = contents.map(move |content| {
                        let seconds = content.position_in_seconds(timeline_tempo);
                        let event = SlotChangeEvent::Continuous {
                            proportional: content.proportional_position().unwrap_or_default(),
                            seconds,
                            peak: content.peak(),
                        };
                        content.notify_pos_changed(temp_project, timeline_tempo, seconds);
                        (row, event)
                    });
                    Either::Left(iter)
                }
                RelevantContent::Recording(runtime_data) => {
                    let event = SlotChangeEvent::Continuous {
                        proportional: runtime_data.proportional_position().unwrap_or_default(),
                        seconds: runtime_data.position_in_seconds_during_recording(timeline_tempo),
                        peak: runtime_data.peak(),
                    };
                    Either::Right(iter::once((row, event)))
                }
            };
            Either::Left(iter)
        });
        change_events.extend(continuous_clip_events);
        change_events
    }

    /// Asynchronously clears the given slot.
    pub fn clear_slot(&self, slot_index: usize) {
        self.rt_command_sender.clear_slot(slot_index);
    }

    /// Freezes the complete column.
    pub async fn freeze(&mut self, _column_index: usize) -> ClipEngineResult<()> {
        let playback_track = self.playback_track()?.clone();
        for (_, slot) in self.slots.iter_mut().enumerate() {
            // TODO-high-clip-matrix implement
            let _ = slot.freeze(&playback_track).await;
        }
        Ok(())
    }

    /// Adds the given clip to the slot or replaces all existing ones.
    pub(crate) fn fill_slot_with_clip(
        &mut self,
        slot_index: usize,
        api_clip: api::Clip,
        chain_equipment: &ChainEquipment,
        recorder_request_sender: &Sender<RecorderRequest>,
        matrix_settings: &MatrixSettings,
        mode: FillClipMode,
    ) -> ClipEngineResult<SlotChangeEvent> {
        let slot = get_slot_mut_insert(&mut self.slots, slot_index);
        if !slot.is_empty() {
            return Err("slot is not empty");
        }
        let clip = Clip::load(api_clip);
        fill_slot_with_clip_internal(
            slot,
            clip,
            chain_equipment,
            recorder_request_sender,
            matrix_settings,
            &self.rt_settings,
            &self.rt_command_sender,
            self.project,
            mode,
        )
    }

    pub(crate) fn replace_slot_contents_with_selected_item(
        &mut self,
        slot_index: usize,
        chain_equipment: &ChainEquipment,
        recorder_request_sender: &Sender<RecorderRequest>,
        matrix_settings: &MatrixSettings,
    ) -> ClipEngineResult<SlotChangeEvent> {
        let item = self
            .project
            .or_current_project()
            .first_selected_item()
            .ok_or("no item selected")?;
        let source = source_util::create_api_source_from_item(item, false)
            .map_err(|_| "couldn't create source from item")?;
        let clip = api::Clip {
            id: None,
            name: None,
            source,
            frozen_source: None,
            active_source: Default::default(),
            // TODO-high Derive whether time or beat from item/track/project
            time_base: ClipTimeBase::Beat(BeatTimeBase {
                // TODO-high Correctly determine audio tempo if audio
                audio_tempo: None,
                // TODO-high Correctly determine time signature at item position
                time_signature: TimeSignature {
                    numerator: 4,
                    denominator: 4,
                },
                // TODO-high Correctly determine by looking at snap offset
                downbeat: PositiveBeat::default(),
            }),
            start_timing: None,
            stop_timing: None,
            // TODO-high Check if item itself is looped or not
            looped: true,
            // TODO-high Derive from item take volume
            volume: api::Db::ZERO,
            // TODO-high Derive from item color
            color: ClipColor::PlayTrackColor,
            // TODO-high Derive from item cut
            section: Section {
                start_pos: PositiveSecond::default(),
                length: None,
            },
            audio_settings: ClipAudioSettings {
                apply_source_fades: true,
                // TODO-high Derive from item time stretch mode
                time_stretch_mode: None,
                // TODO-high Derive from item resample mode
                resample_mode: None,
                cache_behavior: None,
            },
            midi_settings: preferred_clip_midi_settings(),
        };
        self.fill_slot_with_clip(
            slot_index,
            clip,
            chain_equipment,
            recorder_request_sender,
            matrix_settings,
            FillClipMode::Replace,
        )
    }

    pub(crate) fn play_scene(&self, args: ColumnPlayRowArgs) {
        self.rt_command_sender.play_row(args);
    }

    pub(crate) fn play_slot(&self, args: ColumnPlaySlotArgs) {
        self.rt_command_sender.play_slot(args);
    }

    pub(crate) fn stop_slot(&self, args: ColumnStopSlotArgs) {
        self.rt_command_sender.stop_slot(args);
    }

    pub(crate) fn stop(&self, args: ColumnStopArgs) {
        self.rt_command_sender.stop(args);
    }

    pub(crate) fn pause_slot(&self, slot_index: usize) {
        self.rt_command_sender.pause_slot(slot_index);
    }

    pub(crate) fn seek_slot(&self, slot_index: usize, desired_pos: UnitValue) {
        self.rt_command_sender.seek_slot(slot_index, desired_pos);
    }

    pub fn set_clip_volume(
        &mut self,
        slot_index: usize,
        volume: Db,
    ) -> ClipEngineResult<ClipChangeEvent> {
        let slot = get_slot_mut(&mut self.slots, slot_index)?;
        slot.set_volume(volume, &self.rt_command_sender)
    }

    pub fn slots(&self) -> impl Iterator<Item = &Slot> + '_ {
        self.slots.iter()
    }

    /// Returns whether some slots in this column are currently playing/recording.
    pub fn is_stoppable(&self) -> bool {
        self.slots.iter().any(|slot| slot.is_stoppable())
    }

    pub fn is_armed_for_recording(&self) -> bool {
        self.effective_recording_track()
            .map(|t| t.is_armed(true))
            .unwrap_or(false)
    }

    pub fn effective_recording_track(&self) -> ClipEngineResult<Track> {
        let playback_track = self.playback_track()?;
        resolve_recording_track(&self.settings.clip_record_settings, playback_track)
    }

    /// Returns the playback track of this column.
    pub fn playback_track(&self) -> ClipEngineResult<&Track> {
        self.preview_register
            .as_ref()
            .ok_or("column inactive")?
            .track
            .as_ref()
            .ok_or("no playback track set")
    }

    pub fn follows_scene(&self) -> bool {
        self.rt_settings.play_mode.follows_scene()
    }

    pub fn is_recording(&self) -> bool {
        self.slots.iter().any(|s| s.is_recording())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_slot<H: ClipMatrixHandler>(
        &mut self,
        slot_index: usize,
        matrix_record_settings: &MatrixClipRecordSettings,
        chain_equipment: &ChainEquipment,
        recorder_request_sender: &Sender<RecorderRequest>,
        handler: &H,
        containing_track: Option<&Track>,
        overridable_matrix_settings: &OverridableMatrixSettings,
    ) -> ClipEngineResult<()> {
        let recording_track = &self.effective_recording_track()?;
        // Insert slot if it doesn't exist already.
        let slot = get_slot_mut_insert(&mut self.slots, slot_index);
        slot.record_clip(
            matrix_record_settings,
            &self.settings.clip_record_settings,
            &self.rt_settings,
            chain_equipment,
            recorder_request_sender,
            handler,
            containing_track,
            overridable_matrix_settings,
            recording_track,
            &self.rt_column,
            &self.rt_command_sender,
        )
    }
}

impl Drop for PlayingPreviewRegister {
    fn drop(&mut self) {
        self.stop_playing_preview();
    }
}

impl PlayingPreviewRegister {
    pub fn new(source: impl CustomPcmSource + 'static, track: Option<Track>) -> Self {
        let mut register = OwnedPreviewRegister::default();
        register.set_volume(ReaperVolumeValue::ZERO_DB);
        let (out_chan, preview_track) = if let Some(t) = track.as_ref() {
            (-1, Some(t.raw()))
        } else {
            (0, None)
        };
        register.set_out_chan(out_chan);
        register.set_preview_track(preview_track);
        let source = create_custom_owned_pcm_source(source);
        register.set_src(Some(FlexibleOwnedPcmSource::Custom(source)));
        let preview_register = Arc::new(ReaperMutex::new(register));
        let play_handle = start_playing_preview(&preview_register, track.as_ref());
        Self {
            _preview_register: preview_register,
            play_handle,
            track,
        }
    }

    fn stop_playing_preview(&mut self) {
        if let Some(track) = &self.track {
            // Check prevents error message on project close.
            let project = track.project();
            // If not successful this probably means it was stopped already, so okay.
            let _ = Reaper::get()
                .medium_session()
                .stop_track_preview_2(project.context(), self.play_handle);
        } else {
            // If not successful this probably means it was stopped already, so okay.
            let _ = Reaper::get()
                .medium_session()
                .stop_preview(self.play_handle);
        };
    }
}

fn start_playing_preview(
    reg: &SharedRegister,
    track: Option<&Track>,
) -> NonNull<preview_register_t> {
    debug!("Starting preview on track {:?}", &track);
    let buffering_behavior = BitFlags::empty();
    let measure_alignment = MeasureAlignment::PlayImmediately;
    let result = if let Some(track) = track {
        Reaper::get().medium_session().play_track_preview_2_ex(
            track.project().context(),
            reg.clone(),
            buffering_behavior,
            measure_alignment,
        )
    } else {
        panic!("Attempting to initialize column without track. Not yet supported.")
        // Reaper::get().medium_session().play_preview_ex(
        //     reg.clone(),
        //     buffering_behavior,
        //     measure_alignment,
        // )
    };
    result.unwrap()
}

fn get_slot_mut(slots: &mut [Slot], index: usize) -> ClipEngineResult<&mut Slot> {
    slots.get_mut(index).ok_or(SLOT_DOESNT_EXIST)
}

fn get_slot_mut_insert(slots: &mut Vec<Slot>, slot_index: usize) -> &mut Slot {
    upsize_if_necessary(slots, slot_index + 1);
    slots.get_mut(slot_index).unwrap()
}

fn upsize_if_necessary(slots: &mut Vec<Slot>, row_count: usize) {
    let mut current_row_count = slots.len();
    if current_row_count < row_count {
        slots.resize_with(row_count, || {
            let slot = Slot::new(current_row_count);
            current_row_count += 1;
            slot
        });
    }
}

const SLOT_DOESNT_EXIST: &str = "slot doesn't exist";

#[allow(clippy::too_many_arguments)]
fn fill_slot_with_clip_internal(
    slot: &mut Slot,
    mut clip: Clip,
    chain_equipment: &ChainEquipment,
    recorder_request_sender: &Sender<RecorderRequest>,
    matrix_settings: &MatrixSettings,
    column_settings: &rt::ColumnSettings,
    rt_command_sender: &ColumnCommandSender,
    project: Option<Project>,
    mode: FillClipMode,
) -> ClipEngineResult<SlotChangeEvent> {
    let (rt_clip, pooled_midi_source) = clip.create_real_time_clip(
        project,
        chain_equipment,
        recorder_request_sender,
        &matrix_settings.overridable,
        column_settings,
    )?;
    slot.fill_with_clip(clip, &rt_clip, pooled_midi_source, mode);
    let args = ColumnFillSlotArgs {
        slot_index: slot.index(),
        clip: rt_clip,
        mode,
    };
    rt_command_sender.fill_slot_with_clip(Box::new(Some(args)));
    Ok(SlotChangeEvent::Clips("filled slot"))
}

fn resolve_recording_track(
    column_settings: &ColumnClipRecordSettings,
    playback_track: &Track,
) -> ClipEngineResult<Track> {
    if let Some(track_id) = &column_settings.track {
        let track_guid = Guid::from_string_without_braces(track_id.get())?;
        let track = playback_track.project().track_by_guid(&track_guid)?;
        if track.is_available() {
            Ok(track)
        } else {
            Err("track not available")
        }
    } else {
        Ok(playback_track.clone())
    }
}

pub(crate) struct SlotKit<'a> {
    pub slot: &'a mut Slot,
    pub sender: &'a ColumnCommandSender,
}
