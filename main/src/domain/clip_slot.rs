use crate::core::default_util::is_default;
use crate::domain::ClipChangedEvent;
use enumflags2::BitFlags;
use helgoboss_learn::UnitValue;
use reaper_high::{Guid, Item, OwnedSource, Project, Reaper, ReaperSource, Take, Track};
use reaper_low::raw::preview_register_t;
use reaper_low::{add_cpp_pcm_source, raw};
use reaper_medium::{
    create_custom_owned_pcm_source, BufferingBehavior, CustomPcmSource, DurationInBeats,
    DurationInSeconds, ExtGetPooledMidiIdResult, ExtendedArgs, FlexibleOwnedPcmSource,
    GetPeakInfoArgs, GetSamplesArgs, Hz, LoadStateArgs, MeasureAlignment, MediaItem,
    MidiImportBehavior, OwnedPcmSource, OwnedPreviewRegister, PcmSource, PeaksClearArgs, PlayState,
    PositionInSeconds, ProjectContext, ProjectStateContext, PropertiesWindowArgs,
    ReaperFunctionError, ReaperLockError, ReaperMutex, ReaperMutexGuard, ReaperStr,
    ReaperVolumeValue, SaveStateArgs, SetAvailableArgs, SetFileNameArgs, SetSourceArgs,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::Formatter;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::ptr::{null_mut, NonNull};
use std::sync::Arc;
use std::{fmt, mem};

type SharedRegister = Arc<ReaperMutex<OwnedPreviewRegister>>;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SlotDescriptor {
    #[serde(rename = "volume", default, skip_serializing_if = "is_default")]
    pub volume: ReaperVolumeValue,
    #[serde(rename = "repeat", default, skip_serializing_if = "is_default")]
    pub repeat: bool,
    #[serde(rename = "content", default, skip_serializing_if = "is_default")]
    pub content: Option<SlotContent>,
}

impl Default for SlotDescriptor {
    fn default() -> Self {
        Self {
            volume: ReaperVolumeValue::ZERO_DB,
            repeat: false,
            content: None,
        }
    }
}

impl SlotDescriptor {
    pub fn is_filled(&self) -> bool {
        self.content.is_some()
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SlotContent {
    File {
        #[serde(rename = "file")]
        file: PathBuf,
    },
}

impl SlotContent {
    pub fn file(&self) -> Option<&Path> {
        use SlotContent::*;
        match self {
            File { file } => Some(file),
        }
    }

    pub fn create_source(&self, project: Option<Project>) -> Result<OwnedSource, &'static str> {
        match self {
            SlotContent::File { file } => {
                let absolute_file = if file.is_relative() {
                    project
                        .ok_or("slot source given as relative file but without project")?
                        .make_path_absolute(file)
                        .ok_or("couldn't make clip source path absolute")?
                } else {
                    file.clone()
                };
                OwnedSource::from_file(&absolute_file, MidiImportBehavior::UsePreference)
            }
        }
    }
}

#[derive(Debug)]
pub struct ClipSlot {
    descriptor: SlotDescriptor,
    register: SharedRegister,
    state: State,
}

impl Default for ClipSlot {
    fn default() -> Self {
        let descriptor = SlotDescriptor::default();
        let register = create_shared_register(&descriptor);
        Self {
            descriptor,
            register,
            state: State::Empty,
        }
    }
}

fn create_shared_register(descriptor: &SlotDescriptor) -> SharedRegister {
    let mut register = OwnedPreviewRegister::default();
    register.set_volume(descriptor.volume);
    register.set_out_chan(-1);
    Arc::new(ReaperMutex::new(register))
}

impl ClipSlot {
    pub fn descriptor(&self) -> &SlotDescriptor {
        &self.descriptor
    }

    /// Resets all slot data to the defaults (including volume, repeat etc.).
    pub fn reset(&mut self) -> Result<Vec<ClipChangedEvent>, &'static str> {
        self.load(Default::default(), None)
    }

    /// Stops playback if necessary and loads all slot settings including the contained clip from
    /// the given descriptor.
    pub fn load(
        &mut self,
        descriptor: SlotDescriptor,
        project: Option<Project>,
    ) -> Result<Vec<ClipChangedEvent>, &'static str> {
        self.clear()?;
        // Using a completely new register saves us from cleaning up.
        self.register = create_shared_register(&descriptor);
        self.descriptor = descriptor;
        // If we can't load now, don't complain. Maybe media is missing just temporarily. Don't
        // mess up persistent data.
        let _ = self.load_content_from_descriptor(project);
        let events = vec![
            self.play_state_changed_event(),
            self.volume_changed_event(),
            self.repeat_changed_event(),
        ];
        Ok(events)
    }

    fn load_content_from_descriptor(
        &mut self,
        project: Option<Project>,
    ) -> Result<(), &'static str> {
        let source = if let Some(content) = self.descriptor.content.as_ref() {
            content.create_source(project)?
        } else {
            // Nothing to load
            return Ok(());
        };
        self.fill_with_source(source)?;
        Ok(())
    }

    pub fn fill_with_source_from_item(&mut self, item: Item) -> Result<(), Box<dyn Error>> {
        let active_take = item.active_take().ok_or("item has no active take")?;
        let root_source = active_take
            .source()
            .ok_or("take has no source")?
            .root_source();
        let root_source = ReaperSource::new(root_source);
        let source_type = root_source.r#type();
        let item_project = item.project();
        let file = if let Some(source_file) = root_source.file_name() {
            source_file
        } else if source_type == "MIDI" {
            let project = item_project.unwrap_or_else(|| Reaper::get().current_project());
            let recording_path = project.recording_path();
            let take_name = active_take.name();
            let take_name_slug = slug::slugify(take_name);
            let unique_id = nanoid::nanoid!(8);
            let file_name = format!("{}-{}.mid", take_name_slug, unique_id);
            let source_file = recording_path.join(file_name);
            root_source
                .export_to_file(&source_file)
                .map_err(|_| "couldn't export MIDI source to file")?;
            source_file
        } else {
            Err(format!("item source incompatible (type {})", source_type))?
        };
        let content = SlotContent::File {
            file: item_project
                .and_then(|p| p.make_path_relative_if_in_project_directory(&file))
                .unwrap_or(file),
        };
        self.fill_by_user(content, item_project)?;
        Ok(())
    }

    pub fn fill_by_user(
        &mut self,
        content: SlotContent,
        project: Option<Project>,
    ) -> Result<(), &'static str> {
        let source = content.create_source(project)?;
        self.fill_with_source(source)?;
        // Here it's important to not set the descriptor (change things) unless load was successful.
        self.descriptor.content = Some(content);
        Ok(())
    }

    pub fn clip_info(&self) -> Option<ClipInfo> {
        let guard = self.register.lock().ok()?;
        let source = guard.src()?;
        let source = source.as_ref();
        let info = ClipInfo {
            r#type: source.get_type(|t| t.to_string()),
            file_name: source.get_file_name(|p| Some(p?.to_owned())),
            length: source.get_length().ok(),
        };
        // TODO-medium This is probably necessary to make sure the mutex is not unlocked before the
        //  PCM source operations are done. How can we solve this in a better way API-wise? On the
        //  other hand, we are on our own anyway when it comes to PCM source thread safety ...
        std::mem::drop(guard);
        Some(info)
    }

    /// Should be called regularly to detect stops.
    pub fn poll(&mut self) -> Option<ClipChangedEvent> {
        let (result, change_events) = self.start_transition().poll(&self.register);
        self.finish_transition(result);
        change_events
    }

    pub fn is_filled(&self) -> bool {
        self.descriptor.is_filled()
    }

    pub fn source_is_loaded(&self) -> bool {
        !matches!(self.state, State::Empty)
    }

    pub fn play_state(&self) -> ClipPlayState {
        use State::*;
        match &self.state {
            Empty => ClipPlayState::Stopped,
            Suspended(s) => {
                if s.is_paused {
                    ClipPlayState::Paused
                } else {
                    ClipPlayState::Stopped
                }
            }
            Playing(s) => match s.scheduled_for {
                None => ClipPlayState::Playing,
                Some(ScheduledFor::Play) => ClipPlayState::ScheduledForPlay,
                Some(ScheduledFor::Stop) => ClipPlayState::ScheduledForStop,
            },
            Transitioning => unreachable!(),
        }
    }

    pub fn play_state_changed_event(&self) -> ClipChangedEvent {
        ClipChangedEvent::PlayStateChanged(self.play_state())
    }

    fn fill_with_source(&mut self, source: OwnedSource) -> Result<(), &'static str> {
        let result = self
            .start_transition()
            .fill_with_source(source, &self.register);
        self.finish_transition(result)
    }

    pub fn play(
        &mut self,
        track: Option<Track>,
        options: SlotPlayOptions,
    ) -> Result<ClipChangedEvent, &'static str> {
        let result = self.start_transition().play(
            &self.register,
            ClipPlayArgs {
                options,
                track,
                repeat: self.descriptor.repeat,
            },
        );
        self.finish_transition(result)?;
        Ok(self.play_state_changed_event())
    }

    /// Stops playback if necessary, destroys the contained source and resets the playback position
    /// to zero.
    pub fn clear(&mut self) -> Result<(), &'static str> {
        let result = self.start_transition().clear(&self.register);
        self.finish_transition(result)
    }

    pub fn process_transport_change(
        &mut self,
        new_play_state: PlayState,
    ) -> Result<Option<ClipChangedEvent>, &'static str> {
        if !self.descriptor.repeat {
            // One-shots should not be synchronized with main timeline.
            return Ok(None);
        }
        let result = self
            .start_transition()
            .process_transport_change(&self.register, new_play_state);
        self.finish_transition(result)?;
        Ok(Some(self.play_state_changed_event()))
    }

    pub fn stop(&mut self, immediately: bool) -> Result<ClipChangedEvent, &'static str> {
        let result = self.start_transition().stop(&self.register, immediately);
        self.finish_transition(result)?;
        Ok(self.play_state_changed_event())
    }

    pub fn pause(&mut self) -> Result<ClipChangedEvent, &'static str> {
        let result = self.start_transition().pause();
        self.finish_transition(result)?;
        Ok(self.play_state_changed_event())
    }

    pub fn repeat_is_enabled(&self) -> bool {
        self.descriptor.repeat
    }

    pub fn repeat_changed_event(&self) -> ClipChangedEvent {
        ClipChangedEvent::ClipRepeatChanged(self.descriptor.repeat)
    }

    pub fn toggle_repeat(&mut self) -> ClipChangedEvent {
        let new_value = !self.descriptor.repeat;
        self.descriptor.repeat = new_value;
        lock(&self.register).set_looped(new_value);
        self.repeat_changed_event()
    }

    pub fn volume(&self) -> ReaperVolumeValue {
        self.descriptor.volume
    }

    pub fn volume_changed_event(&self) -> ClipChangedEvent {
        ClipChangedEvent::ClipVolumeChanged(self.descriptor.volume)
    }

    pub fn set_volume(&mut self, volume: ReaperVolumeValue) -> ClipChangedEvent {
        self.descriptor.volume = volume;
        lock(&self.register).set_volume(volume);
        self.volume_changed_event()
    }

    pub fn position(&self) -> Result<UnitValue, &'static str> {
        let mut guard = lock(&self.register);
        let source = guard.src().ok_or("no source loaded")?;
        let length = unsafe { source.as_ref().get_length().ok() };
        let position = calculate_proportional_position(guard.cur_pos(), length);
        Ok(position)
    }

    pub fn set_position(&mut self, position: UnitValue) -> Result<ClipChangedEvent, &'static str> {
        let mut guard = lock(&self.register);
        let source = guard.src().ok_or("no source loaded")?;
        let length = unsafe {
            source
                .as_ref()
                .get_length()
                .map_err(|_| "source has no length")?
        };
        let real_pos = PositionInSeconds::new(position.get() * length.get());
        guard.set_cur_pos(real_pos);
        Ok(ClipChangedEvent::ClipPositionChanged(position))
    }

    fn start_transition(&mut self) -> State {
        std::mem::replace(&mut self.state, State::Transitioning)
    }

    fn finish_transition(&mut self, result: TransitionResult) -> Result<(), &'static str> {
        let (next_state, result) = match result {
            Ok(s) => (s, Ok(())),
            Err((s, msg)) => (s, Err(msg)),
        };
        self.state = next_state;
        result
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ClipPlayState {
    Stopped,
    ScheduledForPlay,
    Playing,
    Paused,
    ScheduledForStop,
}

impl ClipPlayState {
    pub fn feedback_value(self) -> UnitValue {
        use ClipPlayState::*;
        match self {
            Stopped => UnitValue::MIN,
            ScheduledForPlay => UnitValue::new(0.25),
            Playing => UnitValue::MAX,
            Paused => UnitValue::new(0.5),
            ScheduledForStop => UnitValue::new(0.75),
        }
    }
}

type TransitionResult = Result<State, (State, &'static str)>;

#[derive(Debug)]
enum State {
    Empty,
    Suspended(SuspendedState),
    Playing(PlayingState),
    Transitioning,
}

impl State {
    pub fn process_transport_change(
        self,
        reg: &SharedRegister,
        new_play_state: PlayState,
    ) -> TransitionResult {
        use State::*;
        match self {
            Suspended(s) if s.was_caused_by_transport_change => {
                if new_play_state.is_playing && !new_play_state.is_paused {
                    if let Some(play_args) = s.last_play_args.clone() {
                        if play_args.options.next_bar {
                            s.play(reg, play_args)
                        } else {
                            Ok(Suspended(s))
                        }
                    } else {
                        Ok(Suspended(s))
                    }
                } else {
                    Ok(Suspended(s))
                }
            }
            Playing(s) if s.args.options.next_bar => {
                if new_play_state.is_playing {
                    Ok(Playing(s))
                } else if new_play_state.is_paused {
                    s.pause(true)
                } else {
                    s.stop(reg, true, true)
                }
            }
            s => Ok(s),
        }
    }

    pub fn play(self, reg: &SharedRegister, args: ClipPlayArgs) -> TransitionResult {
        use State::*;
        match self {
            Empty => Err((Empty, "slot is empty")),
            Suspended(s) => s.play(reg, args),
            Playing(s) => s.play(reg, args),
            Transitioning => unreachable!(),
        }
    }

    pub fn stop(self, reg: &SharedRegister, immediately: bool) -> TransitionResult {
        use State::*;
        match self {
            Empty => Ok(Empty),
            Suspended(s) => s.stop(reg),
            Playing(s) => s.stop(reg, immediately, false),
            Transitioning => unreachable!(),
        }
    }

    pub fn pause(self) -> TransitionResult {
        use State::*;
        match self {
            s @ Empty | s @ Suspended(_) => Ok(s),
            Playing(s) => s.pause(false),
            Transitioning => unreachable!(),
        }
    }

    pub fn clear(self, reg: &SharedRegister) -> TransitionResult {
        use State::*;
        match self {
            Empty => Ok(Empty),
            Suspended(s) => s.clear(reg),
            Playing(s) => s.clear(reg),
            Transitioning => unreachable!(),
        }
    }

    pub fn poll(self, reg: &SharedRegister) -> (TransitionResult, Option<ClipChangedEvent>) {
        use State::*;
        match self {
            Playing(s) => {
                let (current_pos, length, is_looped) = {
                    // React gracefully even in weird situations (because we are in poll).
                    let guard = match reg.lock() {
                        Ok(g) => g,
                        Err(_) => return (Ok(Playing(s)), None),
                    };
                    let source = match guard.src() {
                        Some(s) => s,
                        None => return (Ok(Playing(s)), None),
                    };
                    let length = unsafe { source.as_ref().get_length().ok() };
                    (guard.cur_pos(), length, guard.is_looped())
                };
                let (next_state, event) = match s.scheduled_for {
                    None | Some(ScheduledFor::Stop) if !is_looped => {
                        if let Some(l) = length {
                            if current_pos.get() >= l.get() {
                                // Stop detected. Make it official.
                                (
                                    s.stop(reg, true, false),
                                    Some(ClipChangedEvent::PlayStateChanged(
                                        ClipPlayState::Stopped,
                                    )),
                                )
                            } else {
                                (Ok(Playing(s)), None)
                            }
                        } else {
                            (Ok(Playing(s)), None)
                        }
                    }
                    Some(ScheduledFor::Play) if current_pos.get() > 0.0 => {
                        // Actual play detected. Make it official.
                        let next_playing_state = PlayingState {
                            scheduled_for: None,
                            ..s
                        };
                        (
                            Ok(Playing(next_playing_state)),
                            Some(ClipChangedEvent::PlayStateChanged(ClipPlayState::Playing)),
                        )
                    }
                    _ => (Ok(Playing(s)), None),
                };
                let final_event = event.unwrap_or_else(|| {
                    let position = calculate_proportional_position(current_pos, length);
                    ClipChangedEvent::ClipPositionChanged(position)
                });
                (next_state, Some(final_event))
            }
            _ => (Ok(self), None),
        }
    }

    pub fn fill_with_source(self, source: OwnedSource, reg: &SharedRegister) -> TransitionResult {
        let source = DecoratingPcmSource {
            inner: source.into_raw(),
        };
        let source = create_custom_owned_pcm_source(source);
        let source = FlexibleOwnedPcmSource::Custom(source);
        let source_keeper = SourceKeeper { rust_source: None };
        // let source_keeper = SourceKeeper { rust_source: None };
        // let owned_cpp_decorating_source = source.into_raw();

        // TODO-high Mmh, we need to keep both now, the C++ side (as before) and the Rust
        //  side. In control surface scenario, ReaperSession takes care of both by fully owning.
        //  For PCM sources we need however at least access to the C++ side to be able to pass it
        //  to e.g. preview register functions. That
        //  means it would probably make sense if reaper-medium takes care of holding the Rust Box.
        //  This in turn means that the signature of the ReaperSession method should look like:
        //
        //  INPUT: Box
        //  OUTPUT: OwnedPcmSource
        //
        //  Maybe the best would be if reaper-medium would utilize Arc reference counting to
        //  decide when destroying both sources is okay. We should design the API that way. It
        //  makes sense. As soon as we pass something to REAPER, we can't do what we want anymore
        //  and NEED to leave memory management to reaper-rs, give it at least shared ownership.
        use State::*;
        match self {
            Empty | Suspended(_) => {
                let mut g = lock(reg);
                g.set_src(Some(source));
                g.set_cur_pos(PositionInSeconds::new(0.0));
                Ok(Suspended(SuspendedState {
                    source_keeper,
                    is_paused: false,
                    last_play_args: None,
                    was_caused_by_transport_change: false,
                }))
            }
            Playing(s) => s.fill_with_source(source, source_keeper, reg),
            Transitioning => unreachable!(),
        }
    }
}

struct SourceKeeper {
    rust_source: Option<Box<Box<dyn reaper_low::PCM_source>>>,
}

impl fmt::Debug for SourceKeeper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceKeeper").finish()
    }
}

#[derive(Debug)]
struct SuspendedState {
    source_keeper: SourceKeeper,
    is_paused: bool,
    last_play_args: Option<ClipPlayArgs>,
    was_caused_by_transport_change: bool,
}

#[derive(Clone, Debug)]
struct ClipPlayArgs {
    options: SlotPlayOptions,
    track: Option<Track>,
    repeat: bool,
}

impl SuspendedState {
    pub fn play(self, reg: &SharedRegister, args: ClipPlayArgs) -> TransitionResult {
        {
            let mut guard = lock(reg);
            guard.set_preview_track(args.track.as_ref().map(|t| t.raw()));
            // The looped field might have been reset on non-immediate stop. Set it again.
            guard.set_looped(args.repeat);
        }
        let buffering_behavior = if args.options.is_effectively_buffered() {
            BitFlags::from_flag(BufferingBehavior::BufferSource)
        } else {
            BitFlags::empty()
        };
        let measure_alignment = if args.options.next_bar {
            MeasureAlignment::AlignWithMeasureStart
        } else {
            MeasureAlignment::PlayImmediately
        };
        let result = if let Some(track) = args.track.as_ref() {
            Reaper::get().medium_session().play_track_preview_2_ex(
                track.project().context(),
                reg.clone(),
                buffering_behavior,
                measure_alignment,
            )
        } else {
            Reaper::get().medium_session().play_preview_ex(
                reg.clone(),
                buffering_behavior,
                measure_alignment,
            )
        };
        match result {
            Ok(handle) => {
                let scheduling_state = if args.options.next_bar {
                    Some(ScheduledFor::Play)
                } else {
                    None
                };
                let next_state = PlayingState {
                    source_keeper: self.source_keeper,
                    handle,
                    args,
                    scheduled_for: scheduling_state,
                };
                Ok(State::Playing(next_state))
            }
            Err(_) => Err((State::Suspended(self), "couldn't play preview")),
        }
    }

    pub fn stop(self, reg: &SharedRegister) -> TransitionResult {
        let next_state = State::Suspended(self);
        let mut g = lock(reg);
        // Reset position!
        g.set_cur_pos(PositionInSeconds::new(0.0));
        Ok(next_state)
    }

    pub fn clear(self, reg: &SharedRegister) -> TransitionResult {
        let mut g = lock(reg);
        g.set_src(None);
        g.set_cur_pos(PositionInSeconds::new(0.0));
        Ok(State::Empty)
    }
}

#[derive(Debug)]
struct PlayingState {
    source_keeper: SourceKeeper,
    handle: NonNull<raw::preview_register_t>,
    args: ClipPlayArgs,
    scheduled_for: Option<ScheduledFor>,
}

#[derive(Debug)]
enum ScheduledFor {
    /// Not yet playing but will soon. Final play detection done by polling.
    Play,
    /// Still playing but will stop soon. Final stop detection done by polling.
    Stop,
}

impl PlayingState {
    pub fn play(self, reg: &SharedRegister, args: ClipPlayArgs) -> TransitionResult {
        if self.args.track.as_ref() != args.track.as_ref() {
            // Track change!
            self.suspend(true, false).play(reg, args)
        } else {
            let mut g = lock(reg);
            // Retrigger!
            g.set_cur_pos(PositionInSeconds::new(0.0));
            Ok(State::Playing(self))
        }
    }

    pub fn fill_with_source(
        self,
        source: FlexibleOwnedPcmSource,
        source_keeper: SourceKeeper,
        reg: &SharedRegister,
    ) -> TransitionResult {
        let mut g = lock(reg);
        g.set_src(Some(source));
        Ok(State::Playing(PlayingState {
            source_keeper,
            ..self
        }))
    }

    pub fn stop(
        self,
        reg: &SharedRegister,
        immediately: bool,
        caused_by_transport_change: bool,
    ) -> TransitionResult {
        if immediately {
            let suspended = self.suspend(false, caused_by_transport_change);
            let mut g = lock(reg);
            // Reset position!
            g.set_cur_pos(PositionInSeconds::new(0.0));
            Ok(State::Suspended(suspended))
        } else {
            lock(reg).set_looped(false);
            let next_state = PlayingState {
                scheduled_for: Some(ScheduledFor::Stop),
                ..self
            };
            Ok(State::Playing(next_state))
        }
    }

    pub fn clear(self, reg: &SharedRegister) -> TransitionResult {
        self.suspend(false, false).clear(reg)
    }

    pub fn pause(self, caused_by_transport_change: bool) -> TransitionResult {
        Ok(State::Suspended(
            self.suspend(true, caused_by_transport_change),
        ))
    }

    fn suspend(self, pause: bool, caused_by_transport_change: bool) -> SuspendedState {
        // If not successful this probably means it was stopped already, so okay.
        if let Some(track) = self.args.track.as_ref() {
            let project = track.project();
            // Check prevents error message on project close.
            if project.is_available() {
                let _ = unsafe {
                    Reaper::get()
                        .medium_session()
                        .stop_track_preview_2(project.context(), self.handle)
                };
            }
        } else {
            let _ = unsafe { Reaper::get().medium_session().stop_preview(self.handle) };
        };
        SuspendedState {
            source_keeper: self.source_keeper,
            is_paused: pause,
            last_play_args: Some(self.args),
            was_caused_by_transport_change: caused_by_transport_change,
        }
    }
}

pub struct ClipInfo {
    pub r#type: String,
    pub file_name: Option<PathBuf>,
    pub length: Option<DurationInSeconds>,
}

#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct SlotPlayOptions {
    /// Syncs with timeline.
    pub next_bar: bool,
    pub buffered: bool,
}

impl SlotPlayOptions {
    pub fn is_effectively_buffered(&self) -> bool {
        // Observation: buffered must be on if next bar is enabled.
        self.buffered || self.next_bar
    }
}

fn lock(reg: &SharedRegister) -> ReaperMutexGuard<OwnedPreviewRegister> {
    reg.lock().expect("couldn't acquire lock")
}

fn calculate_proportional_position(
    position: PositionInSeconds,
    length: Option<DurationInSeconds>,
) -> UnitValue {
    if let Some(l) = length {
        if l.get() == 0.0 {
            UnitValue::MIN
        } else {
            UnitValue::new_clamped(position.get() / l.get())
        }
    } else {
        UnitValue::MIN
    }
}

struct DecoratingPcmSource {
    inner: OwnedPcmSource,
}

impl CustomPcmSource for DecoratingPcmSource {
    fn duplicate(&mut self) -> Option<OwnedPcmSource> {
        self.inner.duplicate()
    }

    fn is_available(&mut self) -> bool {
        self.inner.is_available()
    }

    fn set_available(&mut self, args: SetAvailableArgs) {
        self.inner.set_available(args.is_available);
    }

    fn get_type(&mut self) -> &ReaperStr {
        unsafe { self.inner.get_type_unchecked() }
    }

    fn get_file_name(&mut self) -> Option<&ReaperStr> {
        unsafe { self.inner.get_file_name_unchecked() }
    }

    fn set_file_name(&mut self, args: SetFileNameArgs) -> bool {
        self.inner.set_file_name(args.new_file_name)
    }

    fn get_source(&mut self) -> Option<PcmSource> {
        self.inner.get_source()
    }

    fn set_source(&mut self, args: SetSourceArgs) {
        self.inner.set_source(args.source);
    }

    fn get_num_channels(&mut self) -> Option<u32> {
        self.inner.get_num_channels()
    }

    fn get_sample_rate(&mut self) -> Option<Hz> {
        self.inner.get_sample_rate()
    }

    fn get_length(&mut self) -> DurationInSeconds {
        self.inner.get_length().unwrap_or_default()
    }

    fn get_length_beats(&mut self) -> Option<DurationInBeats> {
        self.inner.get_length_beats()
    }

    fn get_bits_per_sample(&mut self) -> u32 {
        self.inner.get_bits_per_sample()
    }

    fn get_preferred_position(&mut self) -> Option<PositionInSeconds> {
        self.inner.get_preferred_position()
    }

    fn properties_window(&mut self, args: PropertiesWindowArgs) -> i32 {
        unsafe { self.inner.properties_window(args.parent_window) }
    }

    fn get_samples(&mut self, args: GetSamplesArgs) {
        unsafe {
            self.inner.get_samples(args.block);
        }
    }

    fn get_peak_info(&mut self, args: GetPeakInfoArgs) {
        unsafe {
            self.inner.get_peak_info(args.block);
        }
    }

    fn save_state(&mut self, args: SaveStateArgs) {
        unsafe {
            self.inner.save_state(args.context);
        }
    }

    fn load_state(&mut self, args: LoadStateArgs) -> Result<(), Box<dyn Error>> {
        unsafe { self.inner.load_state(args.first_line, args.context) }
    }

    fn peaks_clear(&mut self, args: PeaksClearArgs) {
        self.inner.peaks_clear(args.delete_file);
    }

    fn peaks_build_begin(&mut self) -> bool {
        self.inner.peaks_build_begin()
    }

    fn peaks_build_run(&mut self) -> bool {
        self.inner.peaks_build_run()
    }

    fn peaks_build_finish(&mut self) {
        self.inner.peaks_build_finish();
    }

    unsafe fn extended(&self, args: ExtendedArgs) -> i32 {
        unsafe {
            self.inner
                .extended(args.call, args.parm_1, args.parm_2, args.parm_3)
        }
    }
}
