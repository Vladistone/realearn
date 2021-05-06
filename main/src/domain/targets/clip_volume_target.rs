use crate::domain::ui_util::{
    format_as_percentage_without_unit, format_value_as_db, format_value_as_db_without_unit,
    parse_unit_value_from_percentage, parse_value_from_db, reaper_volume_unit_value,
};
use crate::domain::{
    clip_play_state_unit_value, format_value_as_on_off,
    get_control_type_and_character_for_track_exclusivity, handle_track_exclusivity,
    track_arm_unit_value, transport_is_enabled_unit_value, AdditionalFeedbackEvent,
    ClipChangedEvent, ClipPlayState, ControlContext, InstanceFeedbackEvent,
    PlayPosFeedbackResolution, RealearnTarget, SlotPlayOptions, TargetCharacter, TrackExclusivity,
    TransportAction,
};
use helgoboss_learn::{ControlType, ControlValue, Target, UnitValue};
use reaper_high::{ChangeEvent, Project, Track, Volume};

#[derive(Clone, Debug, PartialEq)]
pub struct ClipVolumeTarget {
    pub slot_index: usize,
}

impl RealearnTarget for ClipVolumeTarget {
    fn control_type_and_character(&self) -> (ControlType, TargetCharacter) {
        (ControlType::AbsoluteContinuous, TargetCharacter::Continuous)
    }

    fn parse_as_value(&self, text: &str) -> Result<UnitValue, &'static str> {
        parse_value_from_db(text)
    }

    fn format_value_without_unit(&self, value: UnitValue) -> String {
        format_value_as_db_without_unit(value)
    }

    fn value_unit(&self) -> &'static str {
        "dB"
    }

    fn format_value(&self, value: UnitValue) -> String {
        format_value_as_db(value)
    }

    fn control(&self, value: ControlValue, context: ControlContext) -> Result<(), &'static str> {
        let volume = Volume::try_from_soft_normalized_value(value.as_absolute()?.get());
        let mut instance_state = context.instance_state.borrow_mut();
        instance_state.set_volume(
            self.slot_index,
            volume.unwrap_or(Volume::MIN).reaper_value(),
        )?;
        Ok(())
    }

    fn is_available(&self) -> bool {
        // TODO-medium With clip targets we should check the control context (instance state) if
        //  slot filled.
        true
    }

    fn value_changed_from_instance_feedback_event(
        &self,
        evt: &InstanceFeedbackEvent,
    ) -> (bool, Option<UnitValue>) {
        match evt {
            InstanceFeedbackEvent::ClipChanged {
                slot_index: si,
                event,
            } if *si == self.slot_index => match event {
                ClipChangedEvent::ClipVolumeChanged(new_value) => {
                    (true, Some(reaper_volume_unit_value(*new_value)))
                }
                _ => (false, None),
            },
            _ => (false, None),
        }
    }
}

impl<'a> Target<'a> for ClipVolumeTarget {
    type Context = Option<ControlContext<'a>>;

    fn current_value(&self, context: Option<ControlContext<'a>>) -> Option<UnitValue> {
        let context = context.as_ref()?;
        let instance_state = context.instance_state.borrow();
        let volume = instance_state.get_slot(self.slot_index).ok()?.volume();
        Some(reaper_volume_unit_value(volume))
    }

    fn control_type(&self) -> ControlType {
        self.control_type_and_character().0
    }
}
