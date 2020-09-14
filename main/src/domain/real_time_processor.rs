use crate::domain::{
    CompoundMappingSource, CompoundMappingSourceValue, CompoundMappingTarget, ControlMainTask,
    MappingCompartment, MappingId, MidiClockCalculator, NormalMainTask, PartialControlMatch,
    RealTimeMapping, SourceScanner, VirtualSourceValue,
};
use helgoboss_learn::{ControlValue, MidiSource, MidiSourceValue};
use helgoboss_midi::{
    ControlChange14BitMessage, ControlChange14BitMessageScanner, ParameterNumberMessage,
    ParameterNumberMessageScanner, RawShortMessage, ShortMessage, ShortMessageType,
};
use reaper_high::{MidiInputDevice, MidiOutputDevice, Reaper};
use reaper_medium::{Hz, MidiFrameOffset, SendMidiTime};
use slog::debug;
use std::collections::{HashMap, HashSet};

use enum_iterator::IntoEnumIterator;
use enum_map::{enum_map, EnumMap};
use std::ptr::null_mut;
use vst::api::{EventType, Events, MidiEvent};
use vst::host::Host;
use vst::plugin::HostCallback;

const NORMAL_BULK_SIZE: usize = 100;
const FEEDBACK_BULK_SIZE: usize = 100;

#[derive(PartialEq, Debug)]
pub(crate) enum ControlState {
    Controlling,
    LearningSource,
}

pub struct RealTimeProcessor {
    // Synced processing settings
    pub(crate) control_state: ControlState,
    pub(crate) midi_control_input: MidiControlInput,
    pub(crate) midi_feedback_output: Option<MidiFeedbackOutput>,
    pub(crate) mappings: EnumMap<MappingCompartment, HashMap<MappingId, RealTimeMapping>>,
    pub(crate) let_matched_events_through: bool,
    pub(crate) let_unmatched_events_through: bool,
    // Inter-thread communication
    pub(crate) normal_task_receiver: crossbeam_channel::Receiver<NormalRealTimeTask>,
    pub(crate) feedback_task_receiver: crossbeam_channel::Receiver<FeedbackRealTimeTask>,
    pub(crate) normal_main_task_sender: crossbeam_channel::Sender<NormalMainTask>,
    pub(crate) control_main_task_sender: crossbeam_channel::Sender<ControlMainTask>,
    // Host communication
    pub(crate) host: HostCallback,
    // Scanners for more complex MIDI message types
    pub(crate) nrpn_scanner: ParameterNumberMessageScanner,
    pub(crate) cc_14_bit_scanner: ControlChange14BitMessageScanner,
    // For detecting play state changes
    pub(crate) was_playing_in_last_cycle: bool,
    // For source learning
    pub(crate) source_scanner: SourceScanner,
    // For MIDI timing clock calculations
    pub(crate) midi_clock_calculator: MidiClockCalculator,
}

impl RealTimeProcessor {
    pub fn new(
        normal_task_receiver: crossbeam_channel::Receiver<NormalRealTimeTask>,
        feedback_task_receiver: crossbeam_channel::Receiver<FeedbackRealTimeTask>,
        normal_main_task_sender: crossbeam_channel::Sender<NormalMainTask>,
        control_main_task_sender: crossbeam_channel::Sender<ControlMainTask>,
        host_callback: HostCallback,
    ) -> RealTimeProcessor {
        use MappingCompartment::*;
        RealTimeProcessor {
            control_state: ControlState::Controlling,
            normal_task_receiver,
            feedback_task_receiver,
            normal_main_task_sender,
            control_main_task_sender,
            mappings: enum_map! {
                ControllerMappings => HashMap::with_capacity(100),
                PrimaryMappings => HashMap::with_capacity(500),
            },
            let_matched_events_through: false,
            let_unmatched_events_through: false,
            nrpn_scanner: Default::default(),
            cc_14_bit_scanner: Default::default(),
            midi_control_input: MidiControlInput::FxInput,
            midi_feedback_output: None,
            host: host_callback,
            was_playing_in_last_cycle: false,
            source_scanner: Default::default(),
            midi_clock_calculator: Default::default(),
        }
    }

    pub fn process_incoming_midi_from_fx_input(
        &mut self,
        frame_offset: MidiFrameOffset,
        msg: RawShortMessage,
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
        // Increase MIDI clock calculator's sample counter
        self.midi_clock_calculator
            .increase_sample_counter_by(sample_count as u64);
        // Process occasional tasks sent from other thread (probably main thread)
        let normal_task_count = self.normal_task_receiver.len();
        for task in self.normal_task_receiver.try_iter().take(NORMAL_BULK_SIZE) {
            use NormalRealTimeTask::*;
            match task {
                UpdateAllMappings(compartment, mappings) => {
                    debug!(
                        Reaper::get().logger(),
                        "Real-time processor: Updating all {}...", compartment
                    );
                    for m in mappings.into_iter() {
                        self.mappings[compartment].insert(m.id(), m);
                    }
                }
                UpdateSingleMapping(compartment, mapping) => {
                    debug!(
                        Reaper::get().logger(),
                        "Real-time processor: Updating single {} {:?}...",
                        compartment,
                        mapping.id()
                    );
                    self.mappings[compartment].insert(mapping.id(), mapping);
                }
                EnableMappingsExclusively(compartment, mappings_to_enable) => {
                    // TODO-low We should use an own logger and always log the sample count
                    //  automatically.
                    // Also log sample count in order to be sure about invocation order
                    // (timestamp is not accurate enough on e.g. selection changes).
                    debug!(
                        Reaper::get().logger(),
                        "Real-time processor: Enable {} {} at {} samples...",
                        mappings_to_enable.len(),
                        compartment,
                        self.midi_clock_calculator.current_sample_count()
                    );
                    for m in self.mappings[compartment].values_mut() {
                        m.update_target_activation(mappings_to_enable.contains(&m.id()));
                    }
                }
                UpdateSettings {
                    let_matched_events_through,
                    let_unmatched_events_through,
                    midi_control_input,
                    midi_feedback_output,
                } => {
                    debug!(
                        Reaper::get().logger(),
                        "Real-time processor: Updating settings"
                    );
                    self.let_matched_events_through = let_matched_events_through;
                    self.let_unmatched_events_through = let_unmatched_events_through;
                    self.midi_control_input = midi_control_input;
                    self.midi_feedback_output = midi_feedback_output;
                }
                UpdateSampleRate(sample_rate) => {
                    debug!(
                        Reaper::get().logger(),
                        "Real-time processor: Updating sample rate"
                    );
                    self.midi_clock_calculator.update_sample_rate(sample_rate);
                }
                StartLearnSource => {
                    debug!(
                        Reaper::get().logger(),
                        "Real-time processor: Start learn source"
                    );
                    self.control_state = ControlState::LearningSource;
                    self.nrpn_scanner.reset();
                    self.cc_14_bit_scanner.reset();
                    self.source_scanner.reset();
                }
                StopLearnSource => {
                    debug!(
                        Reaper::get().logger(),
                        "Real-time processor: Stop learn source"
                    );
                    self.control_state = ControlState::Controlling;
                    self.nrpn_scanner.reset();
                    self.cc_14_bit_scanner.reset();
                }
                LogDebugInfo => {
                    self.log_debug_info(normal_task_count);
                }
                UpdateNormalMappingActivations(compartment, activation_updates) => {
                    debug!(
                        Reaper::get().logger(),
                        "Real-time processor: Update mapping activations..."
                    );
                    for update in activation_updates.into_iter() {
                        if let Some(m) = self.mappings[compartment].get_mut(&update.id) {
                            m.update_activation(update.is_active);
                        } else {
                            panic!(
                                "Couldn't find real-time mapping while updating mapping activations"
                            );
                        }
                    }
                }
            }
        }
        // Process (frequent) feedback tasks sent from other thread (probably main thread)
        for task in self
            .feedback_task_receiver
            .try_iter()
            .take(FEEDBACK_BULK_SIZE)
        {
            use FeedbackRealTimeTask::*;
            match task {
                Feedback(source_value) => {
                    use CompoundMappingSourceValue::*;
                    match source_value {
                        Midi(v) => self.feedback_midi(v),
                        Virtual(v) => self.feedback_virtual(v),
                    };
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
                    self.process_incoming_midi(evt.frame_offset(), evt.message().to_other());
                }
            });
        }
        // Poll source scanner if we are learning a source currently
        if self.control_state == ControlState::LearningSource {
            self.poll_source_scanner()
        }
    }

    fn log_debug_info(&self, task_count: usize) {
        let msg = format!(
            "\n\
            # Real-time processor\n\
            \n\
            - State: {:?} \n\
            - Total primary mapping count: {} \n\
            - Enabled primary mapping count: {} \n\
            - Total controller mapping count: {} \n\
            - Enabled controller mapping count: {} \n\
            - Normal task count: {} \n\
            - Feedback task count: {} \n\
            ",
            self.control_state,
            self.mappings[MappingCompartment::PrimaryMappings].len(),
            self.mappings[MappingCompartment::PrimaryMappings]
                .values()
                .filter(|m| m.control_is_effectively_on())
                .count(),
            self.mappings[MappingCompartment::ControllerMappings].len(),
            self.mappings[MappingCompartment::ControllerMappings]
                .values()
                .filter(|m| m.control_is_effectively_on())
                .count(),
            task_count,
            self.feedback_task_receiver.len(),
        );
        Reaper::get()
            .do_in_main_thread_asap(move || {
                Reaper::get().show_console_msg(msg);
            })
            .unwrap();
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

    fn process_incoming_midi(&mut self, frame_offset: MidiFrameOffset, msg: RawShortMessage) {
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
                if let Some(bpm) = self.midi_clock_calculator.feed(frame_offset) {
                    let source_value = MidiSourceValue::<RawShortMessage>::Tempo(bpm);
                    self.control_midi(source_value);
                }
            }
        };
    }

    fn process_incoming_midi_normal(&mut self, msg: RawShortMessage) {
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
        match self.control_state {
            ControlState::Controlling => {
                let matched = self.control_midi(source_value);
                if self.midi_control_input != MidiControlInput::FxInput {
                    return;
                }
                if (matched && self.let_matched_events_through)
                    || (!matched && self.let_unmatched_events_through)
                {
                    for m in msg.to_short_messages::<RawShortMessage>().iter().flatten() {
                        self.forward_midi(*m);
                    }
                }
            }
            ControlState::LearningSource => {
                self.feed_source_scanner(CompoundMappingSourceValue::Midi(source_value));
            }
        }
    }

    fn poll_source_scanner(&mut self) {
        if let Some(source) = self.source_scanner.poll() {
            self.learn_source(source);
        }
    }

    fn feed_source_scanner(&mut self, value: CompoundMappingSourceValue) {
        if let Some(source) = self.source_scanner.feed(value) {
            self.learn_source(source);
        }
    }

    fn learn_source(&mut self, source: CompoundMappingSource) {
        self.normal_main_task_sender
            .send(NormalMainTask::LearnSource(source))
            .unwrap();
    }

    fn process_incoming_midi_normal_cc14(&mut self, msg: ControlChange14BitMessage) {
        let source_value = MidiSourceValue::<RawShortMessage>::ControlChange14Bit(msg);
        match self.control_state {
            ControlState::Controlling => {
                let matched = self.control_midi(source_value);
                if self.midi_control_input != MidiControlInput::FxInput {
                    return;
                }
                if (matched && self.let_matched_events_through)
                    || (!matched && self.let_unmatched_events_through)
                {
                    for m in msg.to_short_messages::<RawShortMessage>().iter() {
                        self.forward_midi(*m);
                    }
                }
            }
            ControlState::LearningSource => {
                self.feed_source_scanner(CompoundMappingSourceValue::Midi(source_value));
            }
        }
    }

    fn process_incoming_midi_normal_plain(&mut self, msg: RawShortMessage) {
        let source_value = MidiSourceValue::Plain(msg);
        match self.control_state {
            ControlState::Controlling => {
                if self.is_consumed(msg) {
                    return;
                }
                let matched = self.control_midi(source_value);
                if matched {
                    self.process_matched_short(msg);
                } else {
                    self.process_unmatched_short(msg);
                }
            }
            ControlState::LearningSource => {
                self.feed_source_scanner(CompoundMappingSourceValue::Midi(source_value));
            }
        }
    }

    fn all_mappings(&self) -> impl Iterator<Item = &RealTimeMapping> {
        MappingCompartment::into_enum_iter()
            .map(move |compartment| self.mappings[compartment].values())
            .flatten()
    }

    fn control_midi_for_compartment(
        &mut self,
        compartment: MappingCompartment,
        value: MidiSourceValue<RawShortMessage>,
    ) -> bool {
        let mut matched = false;
        for m in self.mappings[compartment]
            .values_mut()
            .filter(|m| m.control_is_effectively_on())
        {
            if let Some(control_match) = m.control(value) {
                use PartialControlMatch::*;
                let mapping_matched = match control_match {
                    ProcessVirtual(virtual_source_value) => control_virtual(
                        &self.control_main_task_sender,
                        // TODO-high CONTINUE 2 possibilities to fix this issue:
                        // 1. Collect virtual source values into smallvec and do the iteration
                        //    after releasing self.mappings
                        // 2. Splitting self.mappings into self.mappings and self.virtual_mappings,
                        //    handle the splitting here in this processor. This is a bit harder to
                        //    to do but has the advantage of being faster in general. Less virtual
                        //    controller mappings to iterate? No. Less primary mappings to iterate?
                        //    Actually also no.
                        // 3. Just split into self.mappings and self.controller_mappings. Then:
                        //    2.1 Process self.mappings with MIDI sources (the normal procedure)
                        //    2.2 Process controller mappings. When REAPER target, process normal.
                        //        When virtual target, process self.mappings with virtual sources.
                        &self.mappings,
                        virtual_source_value,
                    ),
                    ForwardToMain(control_value) => {
                        control_main(
                            &self.control_main_task_sender,
                            compartment,
                            m.id(),
                            control_value,
                        );
                        true
                    }
                };
                if mapping_matched {
                    matched = true;
                }
            }
        }
        matched
    }

    /// Returns whether this source value matched one of the mappings.
    fn control_midi(&mut self, value: MidiSourceValue<RawShortMessage>) -> bool {
        self.control_midi_for_compartment(MappingCompartment::ControllerMappings, value)
            | self.control_midi_for_compartment(MappingCompartment::PrimaryMappings, value)
    }

    fn process_matched_short(&self, msg: RawShortMessage) {
        if self.midi_control_input != MidiControlInput::FxInput {
            return;
        }
        if !self.let_matched_events_through {
            return;
        }
        self.forward_midi(msg);
    }

    fn process_unmatched_short(&self, msg: RawShortMessage) {
        if self.midi_control_input != MidiControlInput::FxInput {
            return;
        }
        if !self.let_unmatched_events_through {
            return;
        }
        self.forward_midi(msg);
    }

    fn is_consumed(&self, msg: RawShortMessage) -> bool {
        self.all_mappings()
            .any(|m| m.control_is_effectively_on() && m.consumes(msg))
    }

    fn feedback_midi(&self, value: MidiSourceValue<RawShortMessage>) {
        if let Some(output) = self.midi_feedback_output {
            let shorts = value.to_short_messages();
            if shorts[0].is_none() {
                return;
            }
            match output {
                MidiFeedbackOutput::FxOutput => {
                    for short in shorts.iter().flatten() {
                        self.forward_midi(*short);
                    }
                }
                MidiFeedbackOutput::Device(dev) => {
                    dev.with_midi_output(|mo| {
                        for short in shorts.iter().flatten() {
                            mo.send(*short, SendMidiTime::Instantly);
                        }
                    });
                }
            };
        }
    }

    fn feedback_virtual(&self, value: VirtualSourceValue) {
        if let ControlValue::Absolute(v) = value.control_value() {
            for m in self
                // Only controller mappings can have virtual targets.
                .mappings[MappingCompartment::ControllerMappings]
                .values()
                .filter(|m| m.feedback_is_effectively_on())
            {
                // TODO-low Mmh, very nested
                if let Some(CompoundMappingTarget::Virtual(t)) = m.target() {
                    if t.control_element() == value.control_element() {
                        if let Some(CompoundMappingSourceValue::Midi(midi_value)) =
                            m.source().feedback(v)
                        {
                            self.feedback_midi(midi_value);
                        }
                    }
                }
            }
        }
    }

    fn forward_midi(&self, msg: RawShortMessage) {
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

/// A task which is sent from time to time.
#[derive(Debug)]
pub enum NormalRealTimeTask {
    UpdateAllMappings(MappingCompartment, Vec<RealTimeMapping>),
    UpdateSingleMapping(MappingCompartment, RealTimeMapping),
    UpdateSettings {
        let_matched_events_through: bool,
        let_unmatched_events_through: bool,
        midi_control_input: MidiControlInput,
        midi_feedback_output: Option<MidiFeedbackOutput>,
    },
    /// This takes care of propagating target activation states (right now still mixed up with
    /// enabled/disabled).
    EnableMappingsExclusively(MappingCompartment, HashSet<MappingId>),
    /// Updates the activation state of multiple mappings.
    UpdateNormalMappingActivations(MappingCompartment, Vec<MappingActivationUpdate>),
    LogDebugInfo,
    UpdateSampleRate(Hz),
    StartLearnSource,
    StopLearnSource,
}

#[derive(Copy, Clone, Debug)]
pub struct MappingActivationUpdate {
    pub id: MappingId,
    pub is_active: bool,
}

/// A feedback task (which is potentially sent very frequently).
#[derive(Debug)]
pub enum FeedbackRealTimeTask {
    // TODO-low Is it better for performance to push a vector (smallvec) here?
    Feedback(CompoundMappingSourceValue),
}

impl Drop for RealTimeProcessor {
    fn drop(&mut self) {
        debug!(Reaper::get().logger(), "Dropping real-time processor...");
    }
}

/// MIDI source which provides ReaLearn control data.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum MidiControlInput {
    /// Processes MIDI messages which are fed into ReaLearn FX.
    FxInput,
    /// Processes MIDI messages coming directly from a MIDI input device.
    Device(MidiInputDevice),
}

/// MIDI destination to which ReaLearn's feedback data is sent.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum MidiFeedbackOutput {
    /// Routes feedback messages to the ReaLearn FX output.
    FxOutput,
    /// Routes feedback messages directly to a MIDI output device.
    Device(MidiOutputDevice),
}

fn control_main(
    sender: &crossbeam_channel::Sender<ControlMainTask>,
    compartment: MappingCompartment,
    mapping_id: MappingId,
    value: ControlValue,
) {
    let task = ControlMainTask::Control {
        compartment,
        mapping_id,
        value,
    };
    sender.send(task).unwrap();
}

/// Returns whether this source value matched one of the mappings.
fn control_virtual(
    sender: &crossbeam_channel::Sender<ControlMainTask>,
    mappings: &EnumMap<MappingCompartment, HashMap<MappingId, RealTimeMapping>>,
    value: VirtualSourceValue,
) -> bool {
    // Controller mappings can't have virtual sources, so for now we only need to check
    // primary mappings.
    let compartment = MappingCompartment::PrimaryMappings;
    let mut matched = false;
    for m in mappings[compartment]
        .values()
        .filter(|m| m.control_is_effectively_on())
    {
        if let Some(control_value) = m
            .source()
            .control(&CompoundMappingSourceValue::Virtual(value))
        {
            control_main(sender, compartment, m.id(), control_value);
            matched = true;
        }
    }
    matched
}
