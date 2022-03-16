use crate::rt::supplier::{ChainEquipment, KindSpecificRecordingOutcome, RecorderRequest};
use crate::rt::tempo_util::{calc_tempo_factor, determine_tempo_from_time_base};
use crate::rt::{OverridableMatrixSettings, ProcessingRelevantClipSettings};
use crate::source_util::{
    create_file_api_source, create_pcm_source_from_api_source, CreateApiSourceMode,
};
use crate::{rt, source_util, ClipEngineResult};
use crossbeam_channel::Sender;
use playtime_api as api;
use playtime_api::{ClipColor, Db};
use reaper_high::{OwnedSource, Project};
use reaper_medium::{Bpm, OwnedPcmSource};

#[derive(Clone, Debug)]
pub struct Clip {
    // Unlike Column and Matrix, we use the API clip here directly because there's almost no
    // unnecessary data inside.
    source: api::Source,
    processing_relevant_settings: ProcessingRelevantClipSettings,
    /// `true` for the short moment while recording was requested (using the chain of an existing
    /// clip) but has not yet been acknowledged from a real-time thread.
    recording_requested: bool,
}

impl Clip {
    pub fn load(api_clip: api::Clip) -> Self {
        Self {
            processing_relevant_settings: ProcessingRelevantClipSettings::from_api(&api_clip),
            source: api_clip.source,
            recording_requested: false,
        }
    }

    pub fn from_recording(
        kind_specific_outcome: KindSpecificRecordingOutcome,
        clip_settings: ProcessingRelevantClipSettings,
        temporary_project: Option<Project>,
    ) -> ClipEngineResult<Self> {
        use KindSpecificRecordingOutcome::*;
        let api_source = match kind_specific_outcome {
            Midi { mirror_source } => {
                create_api_source_from_mirror_source(mirror_source, temporary_project)?
            }
            Audio { path, .. } => create_file_api_source(temporary_project, &path),
        };
        let clip = Self {
            source: api_source,
            recording_requested: false,
            processing_relevant_settings: clip_settings,
        };
        Ok(clip)
    }

    pub fn save(&self) -> api::Clip {
        api::Clip {
            source: self.source.clone(),
            time_base: self.processing_relevant_settings.time_base,
            start_timing: self.processing_relevant_settings.start_timing,
            stop_timing: self.processing_relevant_settings.stop_timing,
            looped: self.processing_relevant_settings.looped,
            volume: self.processing_relevant_settings.volume,
            color: ClipColor::PlayTrackColor,
            section: self.processing_relevant_settings.section,
            audio_settings: self.processing_relevant_settings.audio_settings,
            midi_settings: self.processing_relevant_settings.midi_settings,
        }
    }

    pub fn notify_recording_requested(&mut self) -> ClipEngineResult<()> {
        if self.recording_requested {
            return Err("recording has already been requested");
        }
        self.recording_requested = true;
        Ok(())
    }

    pub fn notify_recording_request_acknowledged(&mut self) {
        self.recording_requested = false;
    }

    pub fn notify_midi_overdub_finished(
        &mut self,
        mirror_source: OwnedPcmSource,
        temporary_project: Option<Project>,
    ) -> ClipEngineResult<()> {
        let api_source = create_api_source_from_mirror_source(mirror_source, temporary_project)?;
        self.source = api_source;
        Ok(())
    }

    pub fn notify_recording_canceled(&mut self) {
        // Just in case it hasn't been acknowledged yet.
        self.recording_requested = false;
    }

    pub fn create_real_time_clip(
        &mut self,
        permanent_project: Option<Project>,
        chain_equipment: &ChainEquipment,
        recorder_request_sender: &Sender<RecorderRequest>,
        matrix_settings: &OverridableMatrixSettings,
        column_settings: &rt::ColumnSettings,
    ) -> ClipEngineResult<rt::Clip> {
        rt::Clip::ready(
            &self.source,
            matrix_settings,
            column_settings,
            &self.processing_relevant_settings,
            permanent_project,
            chain_equipment,
            recorder_request_sender,
        )
    }

    pub fn create_mirror_source_for_midi_overdub(
        &self,
        permanent_project: Option<Project>,
    ) -> ClipEngineResult<OwnedPcmSource> {
        create_pcm_source_from_api_source(&self.source, permanent_project)
    }

    pub fn looped(&self) -> bool {
        self.processing_relevant_settings.looped
    }

    pub fn toggle_looped(&mut self) -> bool {
        let looped_new = !self.processing_relevant_settings.looped;
        self.processing_relevant_settings.looped = looped_new;
        looped_new
    }

    pub fn set_volume(&mut self, volume: Db) {
        self.processing_relevant_settings.volume = volume;
    }

    pub fn volume(&self) -> Db {
        self.processing_relevant_settings.volume
    }

    pub fn recording_requested(&self) -> bool {
        self.recording_requested
    }

    pub fn tempo_factor(&self, timeline_tempo: Bpm, is_midi: bool) -> f64 {
        if let Some(tempo) = self.tempo(is_midi) {
            calc_tempo_factor(tempo, timeline_tempo)
        } else {
            1.0
        }
    }

    /// Returns `None` if time base is not "Beat".
    fn tempo(&self, is_midi: bool) -> Option<Bpm> {
        determine_tempo_from_time_base(&self.processing_relevant_settings.time_base, is_midi)
    }
}

fn create_api_source_from_mirror_source(
    mirror_source: OwnedPcmSource,
    temporary_project: Option<Project>,
) -> ClipEngineResult<api::Source> {
    let api_source = source_util::create_api_source_from_pcm_source(
        &OwnedSource::new(mirror_source),
        CreateApiSourceMode::AllowEmbeddedData,
        temporary_project,
    );
    api_source.map_err(|_| "failed creating API source from mirror source")
}
