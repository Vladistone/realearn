use crate::core::MovingAverageCalculator;
use crate::domain::{
    MainProcessorTask, MidiControlInput, MidiSourceScanner, RealTimeProcessorMapping,
};
use helgoboss_learn::{Bpm, MidiSourceValue};
use helgoboss_midi::{
    ControlChange14BitMessage, ControlChange14BitMessageScanner, MessageMainCategory,
    ParameterNumberMessage, ParameterNumberMessageScanner, RawShortMessage, ShortMessage,
    ShortMessageFactory, ShortMessageType, U7,
};
use reaper_high::Reaper;
use reaper_medium::Hz;
use std::ptr::null_mut;
use vst::api::{Event, EventType, Events, MidiEvent, TimeInfo};
use vst::host::Host;
use vst::plugin::HostCallback;

const BULK_SIZE: usize = 1;

pub(crate) enum State {
    Controlling,
    LearningSource,
}

pub struct RealTimeProcessor {
    // Synced processing settings
    pub(crate) state: State,
    pub(crate) midi_control_input: MidiControlInput,
    pub(crate) mappings: Vec<RealTimeProcessorMapping>,
    pub(crate) let_matched_events_through: bool,
    pub(crate) let_unmatched_events_through: bool,
    // Inter-thread communication
    pub(crate) receiver: crossbeam_channel::Receiver<RealTimeProcessorTask>,
    pub(crate) main_processor_sender: crossbeam_channel::Sender<MainProcessorTask>,
    // Host communication
    pub(crate) host: HostCallback,
    // Scanners for more complex MIDI message types
    pub(crate) nrpn_scanner: ParameterNumberMessageScanner,
    pub(crate) cc_14_bit_scanner: ControlChange14BitMessageScanner,
    // For detecting play state changes
    pub(crate) was_playing_in_last_cycle: bool,
    // For source learning
    pub(crate) source_scanner: MidiSourceScanner,
    // For MIDI timing clock calculations
    pub(crate) sample_rate: Hz,
    pub(crate) sample_counter: u64,
    pub(crate) previous_midi_clock_timestamp_in_samples: u64,
    pub(crate) bpm_calculator: MovingAverageCalculator,
}

impl RealTimeProcessor {
    pub fn new(
        receiver: crossbeam_channel::Receiver<RealTimeProcessorTask>,
        main_processor_sender: crossbeam_channel::Sender<MainProcessorTask>,
        host_callback: HostCallback,
    ) -> RealTimeProcessor {
        RealTimeProcessor {
            state: State::Controlling,
            receiver,
            main_processor_sender: main_processor_sender,
            mappings: vec![],
            let_matched_events_through: false,
            let_unmatched_events_through: false,
            nrpn_scanner: Default::default(),
            cc_14_bit_scanner: Default::default(),
            midi_control_input: MidiControlInput::FxInput,
            host: host_callback,
            was_playing_in_last_cycle: false,
            source_scanner: Default::default(),
            sample_rate: Hz::new(1.0),
            sample_counter: 0,
            previous_midi_clock_timestamp_in_samples: 0,
            bpm_calculator: Default::default(),
        }
    }

    // TODO-medium Use better data type for frame_offset as soon as we know the value range
    pub fn process_incoming_midi_from_fx_input(
        &mut self,
        frame_offset: i32,
        msg: impl ShortMessage + Copy,
    ) {
        if self.midi_control_input == MidiControlInput::FxInput {
            let transport_is_starting = !self.was_playing_in_last_cycle && self.is_now_playing();
            if transport_is_starting && msg.r#type() == ShortMessageType::NoteOff {
                // Ignore note off messages which are a result of starting the transport. They
                // are generated by REAPER in order to stop instruments from sounding. But ReaLearn
                // is not an instrument in the classical sense. We don't want to reset target values
                // just because play has been pressed!
                self.process_unmatched_short(msg);
                return;
            }
            self.process_incoming_midi(frame_offset, msg);
        }
    }

    /// Should be called regularly in real-time audio thread.
    pub fn idle(&mut self, sample_count: usize) {
        // Increase our sample counter
        self.sample_counter += sample_count as u64;
        // Process tasks sent from other thread (probably main thread)
        for task in self.receiver.try_iter().take(BULK_SIZE) {
            use RealTimeProcessorTask::*;
            match task {
                UpdateMappings(mappings) => self.mappings = mappings,
                UpdateSettings {
                    let_matched_events_through,
                    let_unmatched_events_through,
                    midi_control_input,
                } => {
                    self.let_matched_events_through = let_matched_events_through;
                    self.let_unmatched_events_through = let_unmatched_events_through;
                    self.midi_control_input = midi_control_input;
                }
                UpdateSampleRate(sample_rate) => {
                    self.sample_rate = sample_rate;
                }
                StartLearnSource => {
                    self.state = State::LearningSource;
                    self.source_scanner.reset();
                }
                StopLearnSource => {
                    self.state = State::Controlling;
                }
            }
        }
        // Get current time information so we can detect changes in play state reliably
        // (TimeInfoFlags::TRANSPORT_CHANGED doesn't work the way we want it).
        self.was_playing_in_last_cycle = self.is_now_playing();
        // Read MIDI events from devices
        if let MidiControlInput::Device(dev) = self.midi_control_input {
            dev.with_midi_input(|mi| {
                for evt in mi.get_read_buf().enum_items(0) {
                    self.process_incoming_midi(evt.frame_offset() as _, evt.message());
                }
            });
        }
    }

    fn is_now_playing(&self) -> bool {
        use vst::api::TimeInfoFlags;
        let time_info = self
            .host
            .get_time_info(TimeInfoFlags::TRANSPORT_PLAYING.bits());
        match time_info {
            None => false,
            Some(ti) => {
                let flags = TimeInfoFlags::from_bits_truncate(ti.flags);
                flags.intersects(TimeInfoFlags::TRANSPORT_PLAYING)
            }
        }
    }

    fn process_incoming_midi(&mut self, frame_offset: i32, msg: impl ShortMessage + Copy) {
        use ShortMessageType::*;
        match msg.r#type() {
            NoteOff
            | NoteOn
            | PolyphonicKeyPressure
            | ControlChange
            | ProgramChange
            | ChannelPressure
            | PitchBendChange
            | Start
            | Continue
            | Stop => {
                self.process_incoming_midi_normal(msg);
            }
            SystemExclusiveStart
            | TimeCodeQuarterFrame
            | SongPositionPointer
            | SongSelect
            | SystemCommonUndefined1
            | SystemCommonUndefined2
            | TuneRequest
            | SystemExclusiveEnd
            | SystemRealTimeUndefined1
            | SystemRealTimeUndefined2
            | ActiveSensing
            | SystemReset => {
                // ReaLearn doesn't process those. Forward them if user wants it.
                self.process_unmatched_short(msg);
            }
            TimingClock => {
                // Timing clock messages are treated special (calculates BPM).
                self.process_incoming_midi_timing_clock(frame_offset, msg);
            }
        };
    }

    fn process_incoming_midi_normal(&mut self, msg: impl ShortMessage + Copy) {
        // TODO-low This is probably unnecessary optimization, but we could switch off NRPN/CC14
        //  scanning if there's no such source.
        if let Some(nrpn_msg) = self.nrpn_scanner.feed(&msg) {
            self.process_incoming_midi_normal_nrpn(nrpn_msg);
        }
        if let Some(cc14_msg) = self.cc_14_bit_scanner.feed(&msg) {
            self.process_incoming_midi_normal_cc14(cc14_msg);
        }
        self.process_incoming_midi_normal_plain(msg);
    }

    fn process_incoming_midi_normal_nrpn(&mut self, msg: ParameterNumberMessage) {
        let source_value = MidiSourceValue::<RawShortMessage>::ParameterNumber(msg);
        match self.state {
            State::Controlling => {
                let matched = self.control(source_value);
                if self.midi_control_input != MidiControlInput::FxInput {
                    return;
                }
                if (matched && self.let_matched_events_through)
                    || (!matched && self.let_unmatched_events_through)
                {
                    for m in msg
                        .to_short_messages::<RawShortMessage>()
                        .into_iter()
                        .flatten()
                    {
                        self.forward_midi(*m);
                    }
                }
            }
            State::LearningSource => {
                self.learn(source_value);
            }
        }
    }

    fn learn(&mut self, value: MidiSourceValue<impl ShortMessage>) {
        if let Some(source) = self.source_scanner.feed(value) {
            let task = MainProcessorTask::LearnSource(source);
            self.main_processor_sender.send(task);
            self.state = State::Controlling;
        }
    }

    fn process_incoming_midi_normal_cc14(&mut self, msg: ControlChange14BitMessage) {
        let source_value = MidiSourceValue::<RawShortMessage>::ControlChange14Bit(msg);
        match self.state {
            State::Controlling => {
                let matched = self.control(source_value);
                if self.midi_control_input != MidiControlInput::FxInput {
                    return;
                }
                if (matched && self.let_matched_events_through)
                    || (!matched && self.let_unmatched_events_through)
                {
                    for m in msg.to_short_messages::<RawShortMessage>().into_iter() {
                        self.forward_midi(*m);
                    }
                }
            }
            State::LearningSource => {
                self.learn(source_value);
            }
        }
    }

    fn process_incoming_midi_normal_plain(&mut self, msg: impl ShortMessage + Copy) {
        let source_value = MidiSourceValue::Plain(msg);
        match self.state {
            State::Controlling => {
                if self.is_consumed(msg) {
                    return;
                }
                let matched = self.control(source_value);
                if matched {
                    self.process_matched_short(msg);
                } else {
                    self.process_unmatched_short(msg);
                }
            }
            State::LearningSource => {
                self.learn(source_value);
            }
        }
    }

    /// Returns whether this source value matched one of the mappings.
    fn control(&self, value: MidiSourceValue<impl ShortMessage>) -> bool {
        let mut matched = false;
        for m in &self.mappings {
            if let Some(control_value) = m.source.control(&value) {
                let main_processor_task = MainProcessorTask::Control {
                    mapping_id: m.mapping_id,
                    value: control_value,
                };
                self.main_processor_sender.send(main_processor_task);
                matched = true;
            }
        }
        matched
    }

    fn process_matched_short(&self, msg: impl ShortMessage) {
        if self.midi_control_input != MidiControlInput::FxInput {
            return;
        }
        if !self.let_matched_events_through {
            return;
        }
        self.forward_midi(msg);
    }

    fn process_unmatched_short(&self, msg: impl ShortMessage) {
        if self.midi_control_input != MidiControlInput::FxInput {
            return;
        }
        if !self.let_unmatched_events_through {
            return;
        }
        self.forward_midi(msg);
    }

    fn is_consumed(&self, msg: impl ShortMessage) -> bool {
        self.mappings.iter().any(|m| m.source.consumes(&msg))
    }

    fn process_incoming_midi_timing_clock(&mut self, frame_offset: i32, msg: impl ShortMessage) {
        // Frame offset is given in 1/1024000 of a second, *not* sample frames!
        let offset_in_secs = frame_offset as f64 / 1024000.0;
        let offset_in_samples = (offset_in_secs * self.sample_rate.get()).round() as u64;
        let timestamp_in_samples = self.sample_counter + offset_in_samples;
        if self.previous_midi_clock_timestamp_in_samples > 0
            && timestamp_in_samples > self.previous_midi_clock_timestamp_in_samples
        {
            let difference_in_samples =
                timestamp_in_samples - self.previous_midi_clock_timestamp_in_samples;
            let difference_in_secs = difference_in_samples as f64 / self.sample_rate.get();
            let num_ticks_per_sec = 1.0 / difference_in_secs;
            let num_beats_per_sec = num_ticks_per_sec / 24.0;
            let num_beats_per_min = num_beats_per_sec * 60.0;
            if num_beats_per_min <= 300.0 {
                self.bpm_calculator.feed(num_beats_per_min);
                if let Some(moving_avg) = self.bpm_calculator.moving_average() {
                    if self.bpm_calculator.value_count_so_far() % 24 == 0 {
                        let source_value =
                            MidiSourceValue::<RawShortMessage>::Tempo(Bpm::new(moving_avg));
                        self.control(source_value);
                    }
                }
            }
        }
        self.previous_midi_clock_timestamp_in_samples = timestamp_in_samples;
    }

    fn forward_midi(&self, msg: impl ShortMessage) {
        let bytes = msg.to_bytes();
        let mut event = MidiEvent {
            event_type: EventType::Midi,
            byte_size: std::mem::size_of::<MidiEvent>() as _,
            delta_frames: 0,
            flags: vst::api::MidiEventFlags::REALTIME_EVENT.bits(),
            note_length: 0,
            note_offset: 0,
            midi_data: [bytes.0, bytes.1.get(), bytes.2.get()],
            _midi_reserved: 0,
            detune: 0,
            note_off_velocity: 0,
            _reserved1: 0,
            _reserved2: 0,
        };
        let events = Events {
            num_events: 1,
            _reserved: 0,
            events: [&mut event as *mut MidiEvent as _, null_mut()],
        };
        self.host.process_events(&events);
    }
}

#[derive(Debug)]
pub enum RealTimeProcessorTask {
    UpdateMappings(Vec<RealTimeProcessorMapping>),
    UpdateSettings {
        let_matched_events_through: bool,
        let_unmatched_events_through: bool,
        midi_control_input: MidiControlInput,
    },
    UpdateSampleRate(Hz),
    StartLearnSource,
    StopLearnSource,
}
