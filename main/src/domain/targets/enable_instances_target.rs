use crate::domain::{
    ControlContext, EnableInstancesArgs, Exclusivity, HitInstructionReturnValue,
    InstanceFeedbackEvent, MappingControlContext, RealearnTarget, TagScope, TargetCharacter,
};
use helgoboss_learn::{AbsoluteValue, ControlType, ControlValue, Target, UnitValue};

#[derive(Clone, Debug, PartialEq)]
pub struct EnableInstancesTarget {
    pub scope: TagScope,
    pub exclusivity: Exclusivity,
}

impl RealearnTarget for EnableInstancesTarget {
    fn control_type_and_character(&self, _: ControlContext) -> (ControlType, TargetCharacter) {
        (
            ControlType::AbsoluteContinuousRetriggerable,
            TargetCharacter::Switch,
        )
    }

    fn hit(
        &mut self,
        value: ControlValue,
        context: MappingControlContext,
    ) -> Result<HitInstructionReturnValue, &'static str> {
        let value = value.to_unit_value()?;
        let is_enable = !value.is_zero();
        let args = EnableInstancesArgs {
            initiator_instance_id: *context.control_context.instance_id,
            initiator_project: context.control_context.processor_context.project(),
            scope: &self.scope,
            is_enable,
            exclusivity: self.exclusivity,
        };
        let tags = context
            .control_context
            .instance_container
            .enable_instances(args);
        let mut instance_state = context.control_context.instance_state.borrow_mut();
        if self.exclusivity == Exclusivity::Exclusive {
            // Completely replace
            let new_active_tags = tags.unwrap_or_else(|| self.scope.tags.clone());
            instance_state.set_active_instance_tags(new_active_tags);
        } else {
            // Add or remove
            instance_state.activate_or_deactivate_instance_tags(&self.scope.tags, is_enable);
        }
        Ok(None)
    }

    fn is_available(&self, _: ControlContext) -> bool {
        true
    }

    fn value_changed_from_instance_feedback_event(
        &self,
        evt: &InstanceFeedbackEvent,
    ) -> (bool, Option<AbsoluteValue>) {
        match evt {
            InstanceFeedbackEvent::ActiveInstanceTagsChanged => (true, None),
            _ => (false, None),
        }
    }
}

impl<'a> Target<'a> for EnableInstancesTarget {
    type Context = ControlContext<'a>;

    fn current_value(&self, context: Self::Context) -> Option<AbsoluteValue> {
        let instance_state = context.instance_state.borrow();
        let active = match self.exclusivity {
            Exclusivity::NonExclusive => {
                instance_state.at_least_those_instance_tags_are_active(&self.scope.tags)
            }
            Exclusivity::Exclusive => {
                instance_state.only_these_instance_tags_are_active(&self.scope.tags)
            }
        };
        let uv = if active {
            UnitValue::MAX
        } else {
            UnitValue::MIN
        };
        Some(AbsoluteValue::Continuous(uv))
    }

    fn control_type(&self, context: Self::Context) -> ControlType {
        self.control_type_and_character(context).0
    }
}
