use core::cmp;
use reaper_medium::{BorrowedPcmSource, PcmSourceTransfer, PositionInSeconds};
use std::ops::Range;

pub trait AudioBuffer {
    fn channel_count(&self) -> usize;

    fn frame_count(&self) -> usize;

    fn interleaved_length(&self) -> usize {
        self.channel_count() * self.channel_count()
    }

    fn data_as_slice(&self) -> &[f64];

    fn data_as_mut_slice(&mut self) -> &mut [f64];

    fn offset_by_mut(&mut self, frame_offset: usize) -> Result<BorrowedAudioBuffer, &'static str> {
        if frame_offset >= self.frame_count() {
            return Err("resulting audio buffer is empty");
        }
        let new_frame_count = self.frame_count() - frame_offset;
        let channel_count = self.channel_count();
        let data_offset = frame_offset * channel_count;
        let offset_buffer = BorrowedAudioBuffer {
            data: &mut self.data_as_mut_slice()[data_offset..],
            frame_count: new_frame_count,
            channel_count: channel_count,
        };
        Ok(offset_buffer)
    }

    fn data_as_mut_ptr(&mut self) -> *mut f64;

    /// `from_src_frame` and `to_dest_frame` are inclusive start frames.
    fn copy_to(
        &self,
        mut dest: impl AudioBuffer,
        from_src_frame: usize,
        to_dest_frame: usize,
        frame_count: usize,
    ) -> Result<(), &'static str> {
        let channel_count = self.channel_count();
        if channel_count != dest.channel_count() {
            return Err("different channel counts");
        }
        if from_src_frame >= self.frame_count() {
            return Err("from_src_frame out of source buffer bounds");
        }
        if to_dest_frame >= dest.frame_count() {
            return Err("to_dest_frame out of destination buffer bounds");
        }
        // Exclusive end
        let src_range_end_frame = from_src_frame + frame_count;
        if src_range_end_frame > self.frame_count() {
            return Err("end of copied range out of source buffer bounds");
        }
        // Exclusive end
        let dest_range_end_frame = to_dest_frame + frame_count;
        if dest_range_end_frame > dest.frame_count() {
            return Err("end of copied range out of destination buffer bounds");
        }
        let start_index = channel_count * from_src_frame;
        let end_index = channel_count * src_range_end_frame;
        let portion = &self.data_as_slice()[start_index..end_index];
        dest.data_as_mut_slice().copy_from_slice(portion);
        Ok(())
    }
}

#[derive(Debug)]
pub struct OwnedAudioBuffer {
    data: Vec<f64>,
    channel_count: usize,
    frame_count: usize,
}

impl OwnedAudioBuffer {
    /// Creates an owned audio buffer with the given topology.
    pub fn new(channel_count: usize, frame_count: usize) -> Self {
        Self {
            data: vec![0.0; channel_count * frame_count],
            channel_count,
            frame_count,
        }
    }

    /// Attempts to create an owned audio buffer with the given topology by reusing the given vec.
    ///
    /// Returns an error if the given vec is not large enough.
    pub fn try_recycle(
        mut data: Vec<f64>,
        channel_count: usize,
        frame_count: usize,
    ) -> Result<Self, &'static str> {
        let min_capacity = channel_count * frame_count;
        if data.capacity() < min_capacity {
            return Err("given vector doesn't have enough capacity");
        }
        data.resize(min_capacity, 0.0);
        let buffer = Self {
            data,
            channel_count,
            frame_count,
        };
        Ok(buffer)
    }

    pub fn into_inner(self) -> Vec<f64> {
        self.data
    }
}

impl AudioBuffer for OwnedAudioBuffer {
    fn frame_count(&self) -> usize {
        self.frame_count
    }

    fn channel_count(&self) -> usize {
        self.channel_count
    }

    fn data_as_mut_ptr(&mut self) -> *mut f64 {
        self.data.as_mut_ptr()
    }

    fn data_as_mut_slice(&mut self) -> &mut [f64] {
        self.data.as_mut_slice()
    }

    fn data_as_slice(&self) -> &[f64] {
        self.data.as_slice()
    }
}

impl<B: AudioBuffer> AudioBuffer for &mut B {
    fn channel_count(&self) -> usize {
        (**self).channel_count()
    }

    fn frame_count(&self) -> usize {
        (**self).frame_count()
    }

    fn data_as_slice(&self) -> &[f64] {
        (**self).data_as_slice()
    }

    fn data_as_mut_slice(&mut self) -> &mut [f64] {
        (**self).data_as_mut_slice()
    }

    fn data_as_mut_ptr(&mut self) -> *mut f64 {
        (**self).data_as_mut_ptr()
    }
}

// TODO-medium Replace this with one of the audio buffer types in the Rust ecosystem
//  (dasp_slice, audio, fon, ...)
#[derive(Debug)]
pub struct BorrowedAudioBuffer<'a> {
    data: &'a mut [f64],
    frame_count: usize,
    channel_count: usize,
}

impl<'a> BorrowedAudioBuffer<'a> {
    pub unsafe fn from_transfer(transfer: &PcmSourceTransfer) -> Self {
        Self::from_raw(
            transfer.samples(),
            transfer.nch() as _,
            transfer.length() as _,
        )
    }

    pub unsafe fn from_raw(data: *mut f64, channel_count: usize, frame_count: usize) -> Self {
        BorrowedAudioBuffer {
            data: unsafe {
                std::slice::from_raw_parts_mut(data, (channel_count * frame_count) as _)
            },
            frame_count,
            channel_count,
        }
    }
}

impl<'a> AudioBuffer for BorrowedAudioBuffer<'a> {
    fn channel_count(&self) -> usize {
        self.channel_count
    }

    fn frame_count(&self) -> usize {
        self.frame_count
    }

    fn data_as_slice(&self) -> &[f64] {
        self.data
    }

    fn data_as_mut_slice(&mut self) -> &mut [f64] {
        self.data
    }

    fn data_as_mut_ptr(&mut self) -> *mut f64 {
        self.data.as_mut_ptr()
    }
}

/// Material to be stretched.
pub trait CopyToAudioBuffer {
    fn copy_to_audio_buffer(
        &self,
        start_frame: usize,
        dest_buffer: impl AudioBuffer,
    ) -> Result<usize, &'static str>;
}

impl<'a> CopyToAudioBuffer for &'a BorrowedPcmSource {
    fn copy_to_audio_buffer(
        &self,
        start_frame: usize,
        mut dest_buffer: impl AudioBuffer,
    ) -> Result<usize, &'static str> {
        let mut transfer = PcmSourceTransfer::default();
        let sample_rate = self.get_sample_rate().ok_or("source without sample rate")?;
        let start_time =
            (start_frame as f64 / sample_rate.get()) % self.get_length().unwrap().get();
        let start_time = PositionInSeconds::new(start_time);
        transfer.set_time_s(start_time);
        transfer.set_sample_rate(sample_rate);
        // TODO-high Here we need to handle repeat/not-repeat
        unsafe {
            transfer.set_nch(dest_buffer.channel_count() as _);
            transfer.set_length(dest_buffer.frame_count() as _);
            transfer.set_samples(dest_buffer.data_as_mut_ptr());
            self.get_samples(&transfer);
        }
        Ok(dest_buffer.frame_count())
    }
}
