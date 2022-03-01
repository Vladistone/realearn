use crate::rt::buffer::AudioBufMut;
use crate::rt::supplier::{
    AudioSupplier, SupplyAudioRequest, SupplyResponse, SupplyResponseStatus, WithFrameRate,
};
use crate::rt::supplier::{
    MidiSupplier, PreBufferFillRequest, PreBufferSourceSkill, SupplyMidiRequest, SupplyRequestInfo,
};
use playtime_api::VirtualResampleMode;
use reaper_high::Reaper;
use reaper_low::raw;
use reaper_medium::{BorrowedMidiEventList, Hz, OwnedReaperResample};
use std::ffi::c_void;
use std::ptr::null_mut;

#[derive(Debug)]
pub struct Resampler<S> {
    enabled: bool,
    responsible_for_audio_time_stretching: bool,
    supplier: S,
    api: OwnedReaperResample,
    tempo_factor: f64,
}

impl<S> Resampler<S> {
    pub fn new(supplier: S) -> Self {
        let api = Reaper::get().medium_reaper().resampler_create();
        Self {
            enabled: false,
            responsible_for_audio_time_stretching: false,
            supplier,
            api,
            tempo_factor: 1.0,
        }
    }

    pub fn reset_buffers_and_latency(&mut self) {
        self.api.as_mut().as_mut().Reset();
    }

    pub fn supplier(&self) -> &S {
        &self.supplier
    }

    pub fn supplier_mut(&mut self) -> &mut S {
        &mut self.supplier
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn set_mode(&mut self, mode: VirtualResampleMode) {
        use VirtualResampleMode::*;
        let raw_mode = match mode {
            ProjectDefault => -1,
            ReaperMode(m) => m.mode as i32,
        };
        unsafe {
            self.api.as_mut().as_mut().Extended(
                raw::RESAMPLE_EXT_SETRSMODE,
                raw_mode as *const c_void as *mut _,
                null_mut(),
                null_mut(),
            );
        }
    }

    /// Decides whether the resampler should also take the tempo factor into account for audio
    /// (VariSpeed).
    pub fn set_responsible_for_audio_time_stretching(&mut self, responsible: bool) {
        self.responsible_for_audio_time_stretching = responsible;
    }

    /// Only has an effect if tempo changing enabled.
    pub fn set_tempo_factor(&mut self, tempo_factor: f64) {
        self.tempo_factor = tempo_factor;
    }
}

impl<S: AudioSupplier + WithFrameRate> AudioSupplier for Resampler<S> {
    fn supply_audio(
        &mut self,
        request: &SupplyAudioRequest,
        dest_buffer: &mut AudioBufMut,
    ) -> SupplyResponse {
        if !self.enabled {
            return self.supplier.supply_audio(request, dest_buffer);
        }
        let source_frame_rate = match self.supplier.frame_rate() {
            None => {
                // Nothing to resample at the moment.
                return self.supplier.supply_audio(request, dest_buffer);
            }
            Some(r) => r,
        };
        let dest_frame_rate = if self.responsible_for_audio_time_stretching {
            Hz::new(request.dest_sample_rate.get() / self.tempo_factor)
        } else {
            request.dest_sample_rate
        };
        if source_frame_rate == dest_frame_rate {
            return self.supplier.supply_audio(request, dest_buffer);
        }
        let mut total_num_frames_consumed = 0usize;
        let mut total_num_frames_written = 0usize;
        let source_channel_count = self.supplier.channel_count();
        let api = self.api.as_mut().as_mut();
        api.SetRates(source_frame_rate.get(), dest_frame_rate.get());
        // Set ResamplePrepare's out_samples to refer to request a specific number of input samples.
        // const RESAMPLE_EXT_SETFEEDMODE: i32 = 0x1001;
        // let ext_result = unsafe {
        //     self.mode.api.Extended(
        //         RESAMPLE_EXT_SETFEEDMODE,
        //         1 as *mut _,
        //         null_mut(),
        //         null_mut(),
        //     )
        // };
        let reached_end = loop {
            // Get resampler buffer.
            let buffer_frame_count = 128usize;
            let mut resample_buffer: *mut f64 = null_mut();
            let num_source_frames_to_write = unsafe {
                api.ResamplePrepare(
                    buffer_frame_count as _,
                    source_channel_count as i32,
                    &mut resample_buffer,
                )
            };
            if num_source_frames_to_write == 0 {
                // We are probably responsible for tempo adjustment and the tempo is super low.
                break false;
            }
            let mut resample_buffer = unsafe {
                AudioBufMut::from_raw(
                    resample_buffer,
                    source_channel_count,
                    num_source_frames_to_write as _,
                )
            };
            // Feed resampler buffer with source material.
            let inner_request = SupplyAudioRequest {
                start_frame: request.start_frame + total_num_frames_consumed as isize,
                dest_sample_rate: source_frame_rate,
                info: SupplyRequestInfo {
                    audio_block_frame_offset: request.info.audio_block_frame_offset
                        + total_num_frames_written,
                    requester: "active-resampler",
                    note: "",
                    is_realtime: false,
                },
                parent_request: Some(request),
                general_info: request.general_info,
            };
            let inner_response = self
                .supplier
                .supply_audio(&inner_request, &mut resample_buffer);
            if inner_response.num_frames_consumed == 0 {
                break true;
            }
            total_num_frames_consumed += inner_response.num_frames_consumed;
            // Get output material.
            let mut offset_buffer = dest_buffer.slice_mut(total_num_frames_written..);
            let num_frames_written = unsafe {
                api.ResampleOut(
                    offset_buffer.data_as_mut_ptr(),
                    num_source_frames_to_write,
                    offset_buffer.frame_count() as _,
                    dest_buffer.channel_count() as _,
                )
            };
            total_num_frames_written += num_frames_written as usize;
            if total_num_frames_written >= dest_buffer.frame_count() {
                // We have enough resampled material.
                break false;
            }
        };
        SupplyResponse {
            num_frames_consumed: total_num_frames_consumed,
            status: if reached_end {
                SupplyResponseStatus::ReachedEnd {
                    num_frames_written: total_num_frames_written,
                }
            } else {
                SupplyResponseStatus::PleaseContinue
            },
        }
    }

    fn channel_count(&self) -> usize {
        self.supplier.channel_count()
    }
}

impl<S: MidiSupplier> MidiSupplier for Resampler<S> {
    fn supply_midi(
        &mut self,
        request: &SupplyMidiRequest,
        event_list: &mut BorrowedMidiEventList,
    ) -> SupplyResponse {
        self.supplier.supply_midi(request, event_list)
    }
}

impl<S: PreBufferSourceSkill + WithFrameRate> PreBufferSourceSkill for Resampler<S> {
    fn pre_buffer(&mut self, request: PreBufferFillRequest) {
        if !self.enabled {
            self.supplier.pre_buffer(request);
            return;
        }
        let source_frame_rate = match self.supplier.frame_rate() {
            None => return self.supplier.pre_buffer(request),
            Some(r) => r,
        };
        let inner_request = PreBufferFillRequest {
            frame_rate: source_frame_rate,
            ..request
        };
        self.supplier.pre_buffer(inner_request);
    }
}

impl<S: WithFrameRate> WithFrameRate for Resampler<S> {
    fn frame_rate(&self) -> Option<Hz> {
        self.supplier.frame_rate()
    }
}