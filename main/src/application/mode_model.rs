use crate::domain::{EelTransformation, Mode, OutputVariable};

use helgoboss_learn::{
    check_mode_applicability, full_discrete_interval, full_unit_interval, AbsoluteMode,
    ButtonUsage, DetailedSourceCharacter, DiscreteIncrement, EncoderUsage, FeedbackType, FireMode,
    GroupInteraction, Interval, ModeApplicabilityCheckInput, ModeParameter, ModeSettings,
    OutOfRangeBehavior, SoftSymmetricUnitValue, TakeoverMode, UnitValue, ValueSequence,
    VirtualColor,
};

use crate::application::{
    Affected, Change, ChangeResult, GetProcessingRelevance, ProcessingRelevance,
};
use std::time::Duration;

pub enum ModeCommand {
    SetAbsoluteMode(AbsoluteMode),
    SetTargetValueInterval(Interval<UnitValue>),
    SetMinTargetValue(UnitValue),
    SetMaxTargetValue(UnitValue),
    SetSourceValueInterval(Interval<UnitValue>),
    SetMinSourceValue(UnitValue),
    SetMaxSourceValue(UnitValue),
    SetReverse(bool),
    SetPressDurationInterval(Interval<Duration>),
    SetMinPressDuration(Duration),
    SetMaxPressDuration(Duration),
    SetTurboRate(Duration),
    SetJumpInterval(Interval<UnitValue>),
    SetMinJump(UnitValue),
    SetMaxJump(UnitValue),
    SetOutOfRangeBehavior(OutOfRangeBehavior),
    SetFireMode(FireMode),
    SetRoundTargetValue(bool),
    SetTakeoverMode(TakeoverMode),
    SetButtonUsage(ButtonUsage),
    SetEncoderUsage(EncoderUsage),
    SetEelControlTransformation(String),
    SetEelFeedbackTransformation(String),
    SetStepInterval(Interval<SoftSymmetricUnitValue>),
    SetMinStep(SoftSymmetricUnitValue),
    SetMaxStep(SoftSymmetricUnitValue),
    SetRotate(bool),
    SetMakeAbsolute(bool),
    SetGroupInteraction(GroupInteraction),
    SetTargetValueSequence(ValueSequence),
    SetFeedbackType(FeedbackType),
    SetTextualFeedbackExpression(String),
    SetFeedbackColor(Option<VirtualColor>),
    SetFeedbackBackgroundColor(Option<VirtualColor>),
    /// This doesn't reset the mode type, just all the values.
    ResetWithinType,
}

#[derive(PartialEq)]
pub enum ModeProp {
    AbsoluteMode,
    TargetValueInterval,
    SourceValueInterval,
    Reverse,
    PressDurationInterval,
    TurboRate,
    JumpInterval,
    OutOfRangeBehavior,
    FireMode,
    RoundTargetValue,
    TakeoverMode,
    ButtonUsage,
    EncoderUsage,
    EelControlTransformation,
    EelFeedbackTransformation,
    StepInterval,
    Rotate,
    MakeAbsolute,
    GroupInteraction,
    TargetValueSequence,
    FeedbackType,
    TextualFeedbackExpression,
    FeedbackColor,
    FeedbackBackgroundColor,
}

impl GetProcessingRelevance for ModeProp {
    fn processing_relevance(&self) -> Option<ProcessingRelevance> {
        // At the moment, all mode aspects are relevant for processing.
        Some(ProcessingRelevance::ProcessingRelevant)
    }
}

/// A model for creating modes
#[derive(Clone, Debug)]
pub struct ModeModel {
    absolute_mode: AbsoluteMode,
    target_value_interval: Interval<UnitValue>,
    source_value_interval: Interval<UnitValue>,
    reverse: bool,
    press_duration_interval: Interval<Duration>,
    turbo_rate: Duration,
    jump_interval: Interval<UnitValue>,
    out_of_range_behavior: OutOfRangeBehavior,
    fire_mode: FireMode,
    round_target_value: bool,
    takeover_mode: TakeoverMode,
    button_usage: ButtonUsage,
    encoder_usage: EncoderUsage,
    eel_control_transformation: String,
    eel_feedback_transformation: String,
    // For relative control values.
    /// Depending on the target character, this is either a step count or a step size.
    ///
    /// A step count is a coefficient which multiplies the atomic step size. E.g. a step count of 2
    /// can be read as 2 * step_size which means double speed. When the step count is negative,
    /// it's interpreted as a fraction of 1. E.g. a step count of -2 is 1/2 * step_size which
    /// means half speed. The increment is fired only every nth time, which results in a
    /// slow-down, or in other words, less sensitivity.
    ///
    /// A step size is the positive, absolute size of an increment. 0.0 represents no increment,
    /// 1.0 represents an increment over the whole value range (not very useful).
    ///
    /// It's an interval. When using rotary encoders, the most important value is the interval
    /// minimum. There are some controllers which deliver higher increments if turned faster. This
    /// is where the maximum comes in. The maximum is also important if using the relative mode
    /// with buttons. The harder you press the button, the higher the increment. It's limited
    /// by the maximum value.
    step_interval: Interval<SoftSymmetricUnitValue>,
    rotate: bool,
    make_absolute: bool,
    group_interaction: GroupInteraction,
    target_value_sequence: ValueSequence,
    feedback_type: FeedbackType,
    textual_feedback_expression: String,
    feedback_color: Option<VirtualColor>,
    feedback_background_color: Option<VirtualColor>,
}

impl Default for ModeModel {
    fn default() -> Self {
        Self {
            absolute_mode: AbsoluteMode::Normal,
            target_value_interval: full_unit_interval(),
            source_value_interval: full_unit_interval(),
            reverse: false,
            press_duration_interval: Interval::new(
                Duration::from_millis(0),
                Duration::from_millis(0),
            ),
            turbo_rate: Duration::from_millis(0),
            jump_interval: full_unit_interval(),
            out_of_range_behavior: Default::default(),
            fire_mode: Default::default(),
            round_target_value: false,
            takeover_mode: Default::default(),
            button_usage: Default::default(),
            encoder_usage: Default::default(),
            eel_control_transformation: String::new(),
            eel_feedback_transformation: String::new(),
            step_interval: Self::default_step_size_interval(),
            rotate: false,
            make_absolute: false,
            group_interaction: Default::default(),
            target_value_sequence: Default::default(),
            feedback_type: Default::default(),
            textual_feedback_expression: Default::default(),
            feedback_color: Default::default(),
            feedback_background_color: Default::default(),
        }
    }
}

impl<'a> Change<'a> for ModeModel {
    type Command = ModeCommand;
    type Prop = ModeProp;

    fn change(&mut self, cmd: ModeCommand) -> Option<Affected<ModeProp>> {
        use Affected::*;
        use ModeCommand as C;
        use ModeProp as P;
        let affected = match cmd {
            C::SetAbsoluteMode(v) => {
                self.absolute_mode = v;
                One(P::AbsoluteMode)
            }
            C::SetTargetValueInterval(v) => {
                self.target_value_interval = v;
                One(P::TargetValueInterval)
            }
            C::SetMinTargetValue(v) => {
                return self.change(C::SetTargetValueInterval(
                    self.target_value_interval.with_min(v),
                ))
            }
            C::SetMaxTargetValue(v) => {
                return self.change(C::SetTargetValueInterval(
                    self.target_value_interval.with_max(v),
                ))
            }
            C::SetSourceValueInterval(v) => {
                self.source_value_interval = v;
                One(P::SourceValueInterval)
            }
            C::SetMinSourceValue(v) => {
                return self.change(C::SetSourceValueInterval(
                    self.source_value_interval.with_min(v),
                ))
            }
            C::SetMaxSourceValue(v) => {
                return self.change(C::SetSourceValueInterval(
                    self.source_value_interval.with_max(v),
                ))
            }
            C::SetReverse(v) => {
                self.reverse = v;
                One(P::Reverse)
            }
            C::SetPressDurationInterval(v) => {
                self.press_duration_interval = v;
                One(P::PressDurationInterval)
            }
            C::SetMinPressDuration(v) => {
                return self.change(C::SetPressDurationInterval(
                    self.press_duration_interval.with_min(v),
                ))
            }
            C::SetMaxPressDuration(v) => {
                return self.change(C::SetPressDurationInterval(
                    self.press_duration_interval.with_max(v),
                ))
            }
            C::SetTurboRate(v) => {
                self.turbo_rate = v;
                One(P::TurboRate)
            }
            C::SetJumpInterval(v) => {
                self.jump_interval = v;
                One(P::JumpInterval)
            }
            C::SetMinJump(v) => {
                return self.change(C::SetJumpInterval(self.jump_interval.with_min(v)))
            }
            C::SetMaxJump(v) => {
                return self.change(C::SetJumpInterval(self.jump_interval.with_max(v)))
            }
            C::SetOutOfRangeBehavior(v) => {
                self.out_of_range_behavior = v;
                One(P::OutOfRangeBehavior)
            }
            C::SetFireMode(v) => {
                self.fire_mode = v;
                One(P::FireMode)
            }
            C::SetRoundTargetValue(v) => {
                self.round_target_value = v;
                One(P::RoundTargetValue)
            }
            C::SetTakeoverMode(v) => {
                self.takeover_mode = v;
                One(P::TakeoverMode)
            }
            C::SetButtonUsage(v) => {
                self.button_usage = v;
                One(P::ButtonUsage)
            }
            C::SetEncoderUsage(v) => {
                self.encoder_usage = v;
                One(P::EncoderUsage)
            }
            C::SetEelControlTransformation(v) => {
                self.eel_control_transformation = v;
                One(P::EelControlTransformation)
            }
            C::SetEelFeedbackTransformation(v) => {
                self.eel_feedback_transformation = v;
                One(P::EelFeedbackTransformation)
            }
            C::SetStepInterval(v) => {
                self.step_interval = v;
                One(P::StepInterval)
            }
            C::SetMinStep(v) => {
                return self.change(C::SetStepInterval(self.step_interval.with_min(v)))
            }
            C::SetMaxStep(v) => {
                return self.change(C::SetStepInterval(self.step_interval.with_max(v)))
            }
            C::SetRotate(v) => {
                self.rotate = v;
                One(P::Rotate)
            }
            C::SetMakeAbsolute(v) => {
                self.make_absolute = v;
                One(P::MakeAbsolute)
            }
            C::SetGroupInteraction(v) => {
                self.group_interaction = v;
                One(P::GroupInteraction)
            }
            C::SetTargetValueSequence(v) => {
                self.target_value_sequence = v;
                One(P::TargetValueSequence)
            }
            C::SetFeedbackType(v) => {
                self.feedback_type = v;
                One(P::FeedbackType)
            }
            C::SetTextualFeedbackExpression(v) => {
                self.textual_feedback_expression = v;
                One(P::TextualFeedbackExpression)
            }
            C::SetFeedbackColor(v) => {
                self.feedback_color = v;
                One(P::FeedbackColor)
            }
            C::SetFeedbackBackgroundColor(v) => {
                self.feedback_background_color = v;
                One(P::FeedbackBackgroundColor)
            }
            C::ResetWithinType => {
                *self = Default::default();
                Multiple
            }
        };
        Some(affected)
    }
}

impl ModeModel {
    pub fn default_step_size_interval() -> Interval<SoftSymmetricUnitValue> {
        // 0.01 has been chosen as default minimum step size because it corresponds to 1%.
        //
        // 0.05 has been chosen as default maximum step size in order to make users aware that
        // ReaLearn supports encoder acceleration ("dial harder = more increments") and
        // velocity-sensitive buttons ("press harder = more increments") but still is low
        // enough to not lead to surprising results such as ugly parameter jumps.
        Interval::new(
            SoftSymmetricUnitValue::new(0.01),
            SoftSymmetricUnitValue::new(0.05),
        )
    }

    pub fn absolute_mode(&self) -> AbsoluteMode {
        self.absolute_mode
    }

    pub fn target_value_interval(&self) -> Interval<UnitValue> {
        self.target_value_interval
    }

    pub fn source_value_interval(&self) -> Interval<UnitValue> {
        self.source_value_interval
    }

    pub fn reverse(&self) -> bool {
        self.reverse
    }

    pub fn press_duration_interval(&self) -> Interval<Duration> {
        self.press_duration_interval
    }

    pub fn turbo_rate(&self) -> Duration {
        self.turbo_rate
    }

    pub fn jump_interval(&self) -> Interval<UnitValue> {
        self.jump_interval
    }

    pub fn out_of_range_behavior(&self) -> OutOfRangeBehavior {
        self.out_of_range_behavior
    }

    pub fn fire_mode(&self) -> FireMode {
        self.fire_mode
    }

    pub fn round_target_value(&self) -> bool {
        self.round_target_value
    }

    pub fn takeover_mode(&self) -> TakeoverMode {
        self.takeover_mode
    }

    pub fn button_usage(&self) -> ButtonUsage {
        self.button_usage
    }

    pub fn encoder_usage(&self) -> EncoderUsage {
        self.encoder_usage
    }

    pub fn eel_control_transformation(&self) -> &str {
        &self.eel_control_transformation
    }

    pub fn eel_feedback_transformation(&self) -> &str {
        &self.eel_feedback_transformation
    }

    pub fn step_interval(&self) -> Interval<SoftSymmetricUnitValue> {
        self.step_interval
    }

    pub fn rotate(&self) -> bool {
        self.rotate
    }

    pub fn make_absolute(&self) -> bool {
        self.make_absolute
    }

    pub fn group_interaction(&self) -> GroupInteraction {
        self.group_interaction
    }

    pub fn target_value_sequence(&self) -> &ValueSequence {
        &self.target_value_sequence
    }

    pub fn feedback_type(&self) -> FeedbackType {
        self.feedback_type
    }

    pub fn textual_feedback_expression(&self) -> &str {
        &self.textual_feedback_expression
    }

    pub fn feedback_color(&self) -> Option<&VirtualColor> {
        self.feedback_color.as_ref()
    }

    pub fn feedback_background_color(&self) -> Option<&VirtualColor> {
        self.feedback_background_color.as_ref()
    }

    pub fn mode_parameter_is_relevant(
        &self,
        mode_parameter: ModeParameter,
        base_input: ModeApplicabilityCheckInput,
        possible_source_characters: &[DetailedSourceCharacter],
        control_is_relevant: bool,
        feedback_is_relevant: bool,
    ) -> bool {
        possible_source_characters.iter().any(|source_character| {
            let is_applicable = |is_feedback| {
                let input = ModeApplicabilityCheckInput {
                    is_feedback,
                    mode_parameter,
                    source_character: *source_character,
                    ..base_input
                };
                check_mode_applicability(input).is_relevant()
            };
            (control_is_relevant && is_applicable(false))
                || (feedback_is_relevant && is_applicable(true))
        })
    }

    /// Creates a mode reflecting this model's current values
    #[allow(clippy::if_same_then_else)]
    pub fn create_mode(
        &self,
        base_input: ModeApplicabilityCheckInput,
        possible_source_characters: &[DetailedSourceCharacter],
    ) -> Mode {
        let is_relevant = |mode_parameter: ModeParameter| {
            // We take both control and feedback into account to not accidentally get slightly
            // different behavior if feedback is not enabled.
            self.mode_parameter_is_relevant(
                mode_parameter,
                base_input,
                possible_source_characters,
                true,
                true,
            )
        };
        // We know that just step max sometimes needs to be set to a sensible default (= step min)
        // and we know that step size and speed is mutually exclusive and therefore doesn't need
        // to be handled separately.
        let step_max_is_relevant =
            is_relevant(ModeParameter::StepSizeMax) || is_relevant(ModeParameter::SpeedMax);
        let min_step_count = convert_to_step_count(self.step_interval.min_val());
        let min_step_size = self.step_interval.min_val().abs();
        Mode::new(ModeSettings {
            absolute_mode: if is_relevant(ModeParameter::AbsoluteMode) {
                self.absolute_mode
            } else {
                AbsoluteMode::default()
            },
            source_value_interval: if is_relevant(ModeParameter::SourceMinMax) {
                self.source_value_interval
            } else {
                full_unit_interval()
            },
            discrete_source_value_interval: if is_relevant(ModeParameter::SourceMinMax) {
                // TODO-high-discrete Use dedicated discrete source interval
                full_discrete_interval()
            } else {
                full_discrete_interval()
            },
            target_value_interval: if is_relevant(ModeParameter::TargetMinMax) {
                self.target_value_interval
            } else {
                full_unit_interval()
            },
            discrete_target_value_interval: if is_relevant(ModeParameter::TargetMinMax) {
                // TODO-high-discrete Use dedicated discrete target interval
                full_discrete_interval()
            } else {
                full_discrete_interval()
            },
            step_count_interval: Interval::new(
                min_step_count,
                if step_max_is_relevant {
                    convert_to_step_count(self.step_interval.max_val())
                } else {
                    min_step_count
                },
            ),
            step_size_interval: Interval::new_auto(
                min_step_size,
                if step_max_is_relevant {
                    self.step_interval.max_val().abs()
                } else {
                    min_step_size
                },
            ),
            jump_interval: if is_relevant(ModeParameter::JumpMinMax) {
                self.jump_interval
            } else {
                full_unit_interval()
            },
            discrete_jump_interval: if is_relevant(ModeParameter::JumpMinMax) {
                // TODO-high-discrete Use dedicated discrete jump interval
                full_discrete_interval()
            } else {
                full_discrete_interval()
            },
            fire_mode: if is_relevant(ModeParameter::FireMode) {
                self.fire_mode
            } else {
                FireMode::default()
            },
            press_duration_interval: self.press_duration_interval,
            turbo_rate: self.turbo_rate,
            takeover_mode: if is_relevant(ModeParameter::TakeoverMode) {
                self.takeover_mode
            } else {
                TakeoverMode::default()
            },
            encoder_usage: if is_relevant(ModeParameter::RelativeFilter) {
                self.encoder_usage
            } else {
                EncoderUsage::default()
            },
            button_usage: if is_relevant(ModeParameter::ButtonFilter) {
                self.button_usage
            } else {
                ButtonUsage::default()
            },
            reverse: if is_relevant(ModeParameter::Reverse) {
                self.reverse
            } else {
                false
            },
            rotate: if is_relevant(ModeParameter::Rotate) {
                self.rotate
            } else {
                false
            },
            round_target_value: if is_relevant(ModeParameter::RoundTargetValue) {
                self.round_target_value
            } else {
                false
            },
            out_of_range_behavior: if is_relevant(ModeParameter::OutOfRangeBehavior) {
                self.out_of_range_behavior
            } else {
                OutOfRangeBehavior::default()
            },
            control_transformation: if is_relevant(ModeParameter::ControlTransformation) {
                EelTransformation::compile(&self.eel_control_transformation, OutputVariable::Y).ok()
            } else {
                None
            },
            feedback_transformation: if is_relevant(ModeParameter::FeedbackTransformation) {
                EelTransformation::compile(&self.eel_feedback_transformation, OutputVariable::X)
                    .ok()
            } else {
                None
            },
            convert_relative_to_absolute: if is_relevant(ModeParameter::MakeAbsolute) {
                self.make_absolute
            } else {
                false
            },
            // TODO-high-discrete Use discrete IF both source and target support it AND enabled
            use_discrete_processing: false,
            target_value_sequence: if is_relevant(ModeParameter::TargetValueSequence) {
                self.target_value_sequence.clone()
            } else {
                Default::default()
            },
            feedback_type: self.feedback_type,
            textual_feedback_expression: if is_relevant(ModeParameter::TextualFeedbackExpression) {
                self.textual_feedback_expression.to_owned()
            } else {
                String::new()
            },
            feedback_color: self.feedback_color.clone(),
            feedback_background_color: self.feedback_background_color.clone(),
        })
    }
}

pub fn convert_factor_to_unit_value(factor: i32) -> SoftSymmetricUnitValue {
    let result = if factor == 0 {
        0.01
    } else {
        factor as f64 / 100.0
    };
    SoftSymmetricUnitValue::new(result)
}

pub fn convert_unit_value_to_factor(value: SoftSymmetricUnitValue) -> i32 {
    // -1.00 => -100
    // -0.01 =>   -1
    //  0.00 =>    1
    //  0.01 =>    1
    //  1.00 =>  100
    let tmp = (value.get() * 100.0).round() as i32;
    if tmp == 0 {
        1
    } else {
        tmp
    }
}

fn convert_to_step_count(value: SoftSymmetricUnitValue) -> DiscreteIncrement {
    DiscreteIncrement::new(convert_unit_value_to_factor(value))
}
