use crate::application::{
    convert_factor_to_unit_value, ActivationConditionCommand, ActivationConditionModel,
    ActivationConditionProp, Affected, Change, GetProcessingRelevance, MappingExtensionModel,
    ModeModel, ProcessingRelevance, SourceModel, TargetCategory, TargetModel,
    TargetModelFormatVeryShort, TargetModelWithContext,
};
use crate::base::{prop, Prop};
use crate::domain::{
    ActivationCondition, CompoundMappingSource, CompoundMappingTarget, ExtendedProcessorContext,
    ExtendedSourceCharacter, FeedbackSendBehavior, GroupId, MainMapping, MappingCompartment,
    MappingId, MappingKey, Mode, PersistentMappingProcessingState, ProcessorMappingOptions,
    QualifiedMappingId, RealearnTarget, ReaperTarget, Tag, TargetCharacter,
    UnresolvedCompoundMappingTarget, VirtualFx, VirtualTrack,
};
use helgoboss_learn::{
    AbsoluteMode, ControlType, DetailedSourceCharacter, Interval, ModeApplicabilityCheckInput,
    ModeParameter, SoftSymmetricUnitValue, SourceCharacter, Target, UnitValue,
};
use rxrust::prelude::*;

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

pub enum MappingCommand {
    SetName(String),
    SetTags(Vec<Tag>),
    SetGroupId(GroupId),
    SetIsEnabled(bool),
    SetControlIsEnabled(bool),
    SetFeedbackIsEnabled(bool),
    SetFeedbackSendBehavior(FeedbackSendBehavior),
    SetVisibleInProjection(bool),
    SetAdvancedSettings(Option<serde_yaml::mapping::Mapping>),
    ChangeActivationCondition(ActivationConditionCommand),
    ClearName,
}

pub enum MappingProp {
    Name,
    Tags,
    GroupId,
    IsEnabled,
    ControlIsEnabled,
    FeedbackIsEnabled,
    FeedbackSendBehavior,
    VisibleInProjection,
    AdvancedSettings,
    InActivationCondition(Affected<ActivationConditionProp>),
}

impl GetProcessingRelevance for MappingProp {
    fn processing_relevance(&self) -> Option<ProcessingRelevance> {
        use MappingProp as P;
        match self {
            P::Name
            | P::Tags
            | P::ControlIsEnabled
            | P::FeedbackIsEnabled
            | P::FeedbackSendBehavior
            | P::VisibleInProjection
            | P::AdvancedSettings => Some(ProcessingRelevance::ProcessingRelevant),
            P::InActivationCondition(p) => p.processing_relevance(),
            P::IsEnabled => Some(ProcessingRelevance::PersistentProcessingRelevant),
            MappingProp::GroupId => {
                // This is handled in different ways.
                None
            }
        }
    }
}

/// A model for creating mappings (a combination of source, mode and target).
#[derive(Clone, Debug)]
pub struct MappingModel {
    id: MappingId,
    key: MappingKey,
    compartment: MappingCompartment,
    name: String,
    tags: Vec<Tag>,
    group_id: GroupId,
    is_enabled: bool,
    control_is_enabled: bool,
    feedback_is_enabled: bool,
    feedback_send_behavior: FeedbackSendBehavior,
    activation_condition_model: ActivationConditionModel,
    visible_in_projection: bool,
    pub source_model: SourceModel,
    pub mode_model: ModeModel,
    pub target_model: TargetModel,
    advanced_settings: Option<serde_yaml::mapping::Mapping>,
    extension_model: MappingExtensionModel,
}

pub type SharedMapping = Rc<RefCell<MappingModel>>;

pub fn share_mapping(mapping: MappingModel) -> SharedMapping {
    Rc::new(RefCell::new(mapping))
}

// We design mapping models as entity (in the DDD sense), so we compare them by ID, not by value.
// Because we store everything in memory instead of working with a database, the memory
// address serves us as ID. That means we just compare pointers.
//
// In all functions which don't need access to the mapping's internal state (comparisons, hashing
// etc.) we use `*const MappingModel` as parameter type because this saves the consumer from
// having to borrow the mapping (when kept in a RefCell). Whenever we can we should compare pointers
// directly, in order to prevent borrowing just to make the following comparison (the RefCell
// comparison internally calls `borrow()`!).
impl PartialEq for MappingModel {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self as _, other as _)
    }
}

fn get_default_target_category_for_compartment(compartment: MappingCompartment) -> TargetCategory {
    use MappingCompartment::*;
    match compartment {
        ControllerMappings => TargetCategory::Virtual,
        MainMappings => TargetCategory::Reaper,
    }
}

impl Change for MappingModel {
    type Command = MappingCommand;
    type Prop = MappingProp;

    fn change(&mut self, cmd: MappingCommand) -> Result<Affected<MappingProp>, String> {
        use Affected::*;
        use MappingCommand as C;
        use MappingProp as P;
        let affected = match cmd {
            C::SetName(v) => {
                self.name = v;
                One(P::Name)
            }
            C::SetTags(v) => {
                self.tags = v;
                One(P::Tags)
            }
            C::SetAdvancedSettings(yaml) => {
                self.advanced_settings = yaml;
                self.update_extension_model_from_advanced_settings()?;
                One(P::AdvancedSettings)
            }
            C::SetGroupId(v) => {
                self.group_id = v;
                One(P::GroupId)
            }
            C::SetIsEnabled(v) => {
                self.is_enabled = v;
                One(P::IsEnabled)
            }
            C::SetControlIsEnabled(v) => {
                self.control_is_enabled = v;
                One(P::ControlIsEnabled)
            }
            C::SetFeedbackIsEnabled(v) => {
                self.feedback_is_enabled = v;
                One(P::FeedbackIsEnabled)
            }
            C::SetFeedbackSendBehavior(v) => {
                self.feedback_send_behavior = v;
                One(P::FeedbackSendBehavior)
            }
            C::SetVisibleInProjection(v) => {
                self.visible_in_projection = v;
                One(P::VisibleInProjection)
            }
            C::ChangeActivationCondition(cmd) => One(P::InActivationCondition(
                self.activation_condition_model.change(cmd)?,
            )),
            C::ClearName => self.change(MappingCommand::SetName(String::new()))?,
        };
        Ok(affected)
    }
}

impl MappingModel {
    pub fn new(
        compartment: MappingCompartment,
        initial_group_id: GroupId,
        key: MappingKey,
    ) -> Self {
        Self {
            id: MappingId::random(),
            key,
            compartment,
            name: Default::default(),
            tags: Default::default(),
            group_id: initial_group_id,
            is_enabled: true,
            control_is_enabled: true,
            feedback_is_enabled: true,
            feedback_send_behavior: Default::default(),
            activation_condition_model: Default::default(),
            visible_in_projection: true,
            source_model: Default::default(),
            mode_model: Default::default(),
            target_model: TargetModel {
                category: prop(get_default_target_category_for_compartment(compartment)),
                ..Default::default()
            },
            advanced_settings: None,
            extension_model: Default::default(),
        }
    }

    pub fn id(&self) -> MappingId {
        self.id
    }

    pub fn key(&self) -> &MappingKey {
        &self.key
    }

    pub fn group_id(&self) -> GroupId {
        self.group_id
    }

    pub fn is_enabled(&self) -> bool {
        self.is_enabled
    }

    pub fn control_is_enabled(&self) -> bool {
        self.control_is_enabled
    }

    pub fn feedback_is_enabled(&self) -> bool {
        self.feedback_is_enabled
    }

    pub fn feedback_send_behavior(&self) -> FeedbackSendBehavior {
        self.feedback_send_behavior
    }

    pub fn visible_in_projection(&self) -> bool {
        self.visible_in_projection
    }

    pub fn activation_condition_model(&self) -> &ActivationConditionModel {
        &self.activation_condition_model
    }

    // TODO-high Replace because it leaks internals and might make one forget about notification.
    pub fn activation_condition_model_mut(&mut self) -> &mut ActivationConditionModel {
        &mut self.activation_condition_model
    }

    pub fn reset_key(&mut self) {
        self.key = MappingKey::random();
    }

    pub fn qualified_id(&self) -> QualifiedMappingId {
        QualifiedMappingId::new(self.compartment, self.id)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tags(&self) -> &[Tag] {
        &self.tags
    }

    pub fn effective_name(&self) -> String {
        if self.name.is_empty() {
            TargetModelFormatVeryShort(&self.target_model).to_string()
        } else {
            self.name.clone()
        }
    }

    pub fn make_project_independent(&mut self, context: ExtendedProcessorContext) {
        let compartment = self.compartment();
        let target = &mut self.target_model;
        match target.category.get() {
            TargetCategory::Reaper => {
                let changed_to_track_ignore_fx = if target.supports_fx() {
                    let refers_to_project = target.fx_type.get().refers_to_project();
                    if refers_to_project {
                        let target_with_context = target.with_context(context, compartment);
                        let virtual_fx = if target_with_context.first_fx().ok().as_ref()
                            == Some(context.context().containing_fx())
                        {
                            // This is ourselves!
                            VirtualFx::This
                        } else {
                            VirtualFx::Focused
                        };
                        target.set_virtual_fx(virtual_fx, context, compartment);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if target.r#type.get().supports_track()
                    && target.track_type.get().refers_to_project()
                {
                    let new_virtual_track = if changed_to_track_ignore_fx {
                        // Track doesn't matter at all. We change it to <This>. Looks nice.
                        Some(VirtualTrack::This)
                    } else if let Ok(t) = target
                        .with_context(context, compartment)
                        .first_effective_track()
                    {
                        t.index().map(VirtualTrack::ByIndex)
                    } else {
                        None
                    };
                    if let Some(t) = new_virtual_track {
                        target.set_virtual_track(t, Some(context.context()));
                    }
                }
            }
            TargetCategory::Virtual => {}
        }
    }

    pub fn make_target_sticky(
        &mut self,
        context: ExtendedProcessorContext,
    ) -> Result<(), Box<dyn Error>> {
        let target = &mut self.target_model;
        match target.category.get() {
            TargetCategory::Reaper => {
                if target.supports_track() {
                    target.make_track_sticky(self.compartment, context)?;
                }
                if target.supports_fx() {
                    target.make_fx_sticky(self.compartment, context)?;
                }
                if target.supports_route() {
                    target.make_route_sticky(self.compartment, context)?;
                }
            }
            TargetCategory::Virtual => {}
        }
        Ok(())
    }

    pub fn advanced_settings(&self) -> Option<&serde_yaml::Mapping> {
        self.advanced_settings.as_ref()
    }

    fn update_extension_model_from_advanced_settings(&mut self) -> Result<(), String> {
        // Immediately update extension model
        let extension_model = if let Some(yaml_mapping) = self.advanced_settings() {
            serde_yaml::from_value(serde_yaml::Value::Mapping(yaml_mapping.clone()))
                .map_err(|e| e.to_string())?
        } else {
            Default::default()
        };
        self.extension_model = extension_model;
        Ok(())
    }

    pub fn duplicate(&self) -> MappingModel {
        MappingModel {
            id: MappingId::random(),
            key: MappingKey::random(),
            ..self.clone()
        }
    }

    pub fn compartment(&self) -> MappingCompartment {
        self.compartment
    }

    pub fn with_context<'a>(
        &'a self,
        context: ExtendedProcessorContext<'a>,
    ) -> MappingModelWithContext<'a> {
        MappingModelWithContext {
            mapping: self,
            context,
        }
    }

    pub fn adjust_mode_if_necessary(&mut self, context: ExtendedProcessorContext) {
        let with_context = self.with_context(context);
        if with_context.mode_makes_sense() == Ok(false) {
            if let Ok(preferred_mode_type) = with_context.preferred_mode_type() {
                self.mode_model.r#type.set(preferred_mode_type);
                self.set_preferred_mode_values(context);
            }
        }
    }

    pub fn reset_mode(&mut self, context: ExtendedProcessorContext) {
        self.mode_model.reset_within_type();
        self.set_preferred_mode_values(context);
    }

    // Changes mode settings if there are some preferred ones for a certain source or target.
    pub fn set_preferred_mode_values(&mut self, context: ExtendedProcessorContext) {
        self.mode_model
            .step_interval
            .set(self.with_context(context).preferred_step_interval())
    }

    /// Fires whenever a property has changed that has an effect on control/feedback processing.
    ///
    /// However, we don't include properties here which are changed by the processing layer
    /// (such as `is_enabled`) because that would mean the complete mapping will be synced as a
    /// result, whereas we want to sync processing stuff faster!  
    pub fn changed_processing_relevant(
        &self,
    ) -> impl LocalObservable<'static, Item = (), Err = ()> + 'static {
        self.source_model
            .changed()
            .merge(self.mode_model.changed())
            .merge(self.target_model.changed())
    }

    pub fn base_mode_applicability_check_input(&self) -> ModeApplicabilityCheckInput {
        ModeApplicabilityCheckInput {
            target_is_virtual: self.target_model.is_virtual(),
            // TODO-high-discrete Enable (also taking source into consideration!)
            target_supports_discrete_values: false,
            is_feedback: false,
            make_absolute: self.mode_model.make_absolute.get(),
            use_textual_feedback: self.mode_model.feedback_type.get().is_textual(),
            // Any is okay, will be overwritten.
            source_character: DetailedSourceCharacter::RangeControl,
            absolute_mode: self.mode_model.r#type.get(),
            // Any is okay, will be overwritten.
            mode_parameter: ModeParameter::TargetMinMax,
            target_value_sequence_is_set: !self
                .mode_model
                .target_value_sequence
                .get_ref()
                .is_empty(),
        }
    }

    pub fn control_is_enabled_and_supported(&self) -> bool {
        self.control_is_enabled()
            && self.source_model.supports_control()
            && self.target_model.supports_control()
    }

    pub fn feedback_is_enabled_and_supported(&self) -> bool {
        self.feedback_is_enabled()
            && self.source_model.supports_feedback()
            && self.target_model.supports_feedback()
    }

    pub fn mode_parameter_is_relevant(
        &self,
        mode_parameter: ModeParameter,
        base_input: ModeApplicabilityCheckInput,
        possible_source_characters: &[DetailedSourceCharacter],
    ) -> bool {
        self.mode_model.mode_parameter_is_relevant(
            mode_parameter,
            base_input,
            possible_source_characters,
            self.control_is_enabled_and_supported(),
            self.feedback_is_enabled_and_supported(),
        )
    }

    fn create_source(&self) -> CompoundMappingSource {
        self.source_model.create_source()
    }

    fn create_mode(&self) -> Mode {
        let possible_source_characters = self.source_model.possible_detailed_characters();
        self.mode_model.create_mode(
            self.base_mode_applicability_check_input(),
            &possible_source_characters,
        )
    }

    fn create_target(&self) -> Option<UnresolvedCompoundMappingTarget> {
        self.target_model.create_target(self.compartment).ok()
    }

    pub fn create_persistent_mapping_processing_state(&self) -> PersistentMappingProcessingState {
        PersistentMappingProcessingState {
            is_enabled: self.is_enabled(),
        }
    }

    /// Creates an intermediate mapping for splintering into very dedicated mapping types that are
    /// then going to be distributed to real-time and main processor.
    pub fn create_main_mapping(&self, group_data: GroupData) -> MainMapping {
        let id = self.id;
        let source = self.create_source();
        let mode = self.create_mode();
        let unresolved_target = self.create_target();
        let activation_condition = self
            .activation_condition_model
            .create_activation_condition();
        let options = ProcessorMappingOptions {
            // TODO-medium Encapsulate, don't set here
            target_is_active: false,
            persistent_processing_state: self.create_persistent_mapping_processing_state(),
            control_is_enabled: group_data.control_is_enabled && self.control_is_enabled(),
            feedback_is_enabled: group_data.feedback_is_enabled && self.feedback_is_enabled(),
            feedback_send_behavior: self.feedback_send_behavior(),
        };
        let mut merged_tags = group_data.tags;
        merged_tags.extend_from_slice(&self.tags);
        MainMapping::new(
            self.compartment,
            id,
            &self.key,
            self.group_id(),
            self.name.clone(),
            merged_tags,
            source,
            mode,
            self.mode_model.group_interaction.get(),
            unresolved_target,
            group_data.activation_condition,
            activation_condition,
            options,
            self.extension_model
                .create_mapping_extension()
                .unwrap_or_default(),
        )
    }
}

pub struct GroupData {
    pub control_is_enabled: bool,
    pub feedback_is_enabled: bool,
    pub activation_condition: ActivationCondition,
    pub tags: Vec<Tag>,
}

impl Default for GroupData {
    fn default() -> Self {
        Self {
            control_is_enabled: true,
            feedback_is_enabled: true,
            activation_condition: ActivationCondition::Always,
            tags: vec![],
        }
    }
}

pub struct MappingModelWithContext<'a> {
    mapping: &'a MappingModel,
    context: ExtendedProcessorContext<'a>,
}

impl<'a> MappingModelWithContext<'a> {
    pub fn mode_makes_sense(&self) -> Result<bool, &'static str> {
        use ExtendedSourceCharacter::*;
        use SourceCharacter::*;
        let mode_type = self.mapping.mode_model.r#type.get();
        let result = match self.mapping.source_model.character() {
            Normal(RangeElement) => mode_type == AbsoluteMode::Normal,
            Normal(MomentaryButton) | Normal(ToggleButton) => {
                let target = self.target_with_context().resolve_first()?;
                match mode_type {
                    AbsoluteMode::Normal | AbsoluteMode::ToggleButton => !target
                        .control_type(self.context.control_context())
                        .is_relative(),
                    AbsoluteMode::IncrementalButton => {
                        if target
                            .control_type(self.context.control_context())
                            .is_relative()
                        {
                            true
                        } else {
                            match target.character(self.context.control_context()) {
                                TargetCharacter::Discrete
                                | TargetCharacter::Continuous
                                | TargetCharacter::VirtualMulti => true,
                                TargetCharacter::Trigger
                                | TargetCharacter::Switch
                                | TargetCharacter::VirtualButton => false,
                            }
                        }
                    }
                }
            }
            Normal(Encoder1) | Normal(Encoder2) | Normal(Encoder3) => true,
            VirtualContinuous => true,
        };
        Ok(result)
    }

    pub fn has_target(&self, target: &ReaperTarget) -> bool {
        self.target_with_context()
            .resolve()
            .iter()
            .flatten()
            .any(|t| match t {
                CompoundMappingTarget::Reaper(t) => t == target,
                _ => false,
            })
    }

    pub fn preferred_mode_type(&self) -> Result<AbsoluteMode, &'static str> {
        use ExtendedSourceCharacter::*;
        use SourceCharacter::*;
        let result = match self.mapping.source_model.character() {
            Normal(RangeElement) | VirtualContinuous => AbsoluteMode::Normal,
            Normal(MomentaryButton) | Normal(ToggleButton) => {
                let target = self.target_with_context().resolve_first()?;
                if target
                    .control_type(self.context.control_context())
                    .is_relative()
                {
                    AbsoluteMode::IncrementalButton
                } else {
                    match target.character(self.context.control_context()) {
                        TargetCharacter::Trigger
                        | TargetCharacter::Continuous
                        | TargetCharacter::VirtualMulti => AbsoluteMode::Normal,
                        TargetCharacter::Switch | TargetCharacter::VirtualButton => {
                            AbsoluteMode::ToggleButton
                        }
                        TargetCharacter::Discrete => AbsoluteMode::IncrementalButton,
                    }
                }
            }
            Normal(Encoder1) | Normal(Encoder2) | Normal(Encoder3) => AbsoluteMode::Normal,
        };
        Ok(result)
    }

    pub fn uses_step_counts(&self) -> bool {
        let mode = self.mapping.create_mode();
        if mode.settings().convert_relative_to_absolute {
            // If we convert increments to absolute values, we want step sizes of course.
            return false;
        }
        if !mode.settings().target_value_sequence.is_empty() {
            // If we have a target value sequence, we are discrete all the way!
            return true;
        }
        let target = match self.target_with_context().resolve_first().ok() {
            None => return false,
            Some(t) => t,
        };
        match target.control_type(self.context.control_context()) {
            ControlType::AbsoluteContinuousRetriggerable => false,
            ControlType::AbsoluteContinuous => false,
            ControlType::AbsoluteContinuousRoundable { .. } => false,
            ControlType::AbsoluteDiscrete { .. } => true,
            ControlType::Relative => true,
            ControlType::VirtualMulti => true,
            ControlType::VirtualButton => false,
        }
    }

    fn preferred_step_interval(&self) -> Interval<SoftSymmetricUnitValue> {
        if self.uses_step_counts() {
            let one_step = convert_factor_to_unit_value(1);
            Interval::new(one_step, one_step)
        } else {
            match self.target_step_size() {
                Some(step_size) => {
                    Interval::new(step_size.to_symmetric(), step_size.to_symmetric())
                }
                None => ModeModel::default_step_size_interval(),
            }
        }
    }

    fn target_step_size(&self) -> Option<UnitValue> {
        let target = self.target_with_context().resolve_first().ok()?;
        target
            .control_type(self.context.control_context())
            .step_size()
    }

    fn target_with_context(&self) -> TargetModelWithContext<'_> {
        self.mapping
            .target_model
            .with_context(self.context, self.mapping.compartment)
    }
}
