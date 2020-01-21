/// event_mapper::movement watches all local entities movements and determines which
/// sfx to emit, and the position at which the sound should be emitted from
use crate::audio::sfx::{SfxTriggerItem, SfxTriggers};

use client::Client;
use common::{
    comp::{Body, CharacterState, Pos},
    event::{EventBus, SfxEvent, SfxEventItem},
};
use hashbrown::HashMap;
use specs::{Entity as EcsEntity, Join, WorldExt};
use std::time::{Duration, Instant};
use vek::*;

#[derive(Clone)]
struct LastSfxEvent {
    event: SfxEvent,
    time: Instant,
}

pub struct MovementEventMapper {
    event_history: HashMap<EcsEntity, LastSfxEvent>,
}

impl MovementEventMapper {
    pub fn new() -> Self {
        Self {
            event_history: HashMap::new(),
        }
    }

    pub fn maintain(&mut self, client: &Client, triggers: &SfxTriggers) {
        const SFX_DIST_LIMIT_SQR: f32 = 22500.0;
        let ecs = client.state().ecs();

        let player_position = ecs
            .read_storage::<Pos>()
            .get(client.entity())
            .map_or(Vec3::zero(), |pos| pos.0);

        for (entity, pos, body, character) in (
            &ecs.entities(),
            &ecs.read_storage::<Pos>(),
            &ecs.read_storage::<Body>(),
            ecs.read_storage::<CharacterState>().maybe(),
        )
            .join()
            .filter(|(_, e_pos, ..)| {
                (e_pos.0.distance_squared(player_position)) < SFX_DIST_LIMIT_SQR
            })
        {
            if let Some(character) = character {
                let state = self
                    .event_history
                    .entry(entity)
                    .or_insert_with(|| LastSfxEvent {
                        event: SfxEvent::Idle,
                        time: Instant::now(),
                    });

                let mapped_event = match body {
                    Body::Humanoid(_) => Self::map_movement_event(character, state.event.clone()),
                    Body::QuadrupedMedium(_) => {
                        // TODO: Quadriped running sfx
                        SfxEvent::Idle
                    }
                    _ => SfxEvent::Idle,
                };

                // Check for SFX config entry for this movement
                if Self::should_emit(state, triggers.get_key_value(&mapped_event)) {
                    ecs.read_resource::<EventBus<SfxEventItem>>()
                        .emitter()
                        .emit(SfxEventItem::new(mapped_event, Some(pos.0)));

                    // Update the last play time
                    state.event = mapped_event;
                    state.time = Instant::now();
                } else {
                    // Keep the last event, it may not have an SFX trigger but it helps us determine the next one
                    state.event = mapped_event;
                }
            }
        }

        self.cleanup(client.entity());
    }

    /// As the player explores the world, we track the last event of the nearby entities to determine the correct
    /// SFX item to play next based on their activity. `cleanup` will remove entities from event tracking if they
    /// have not triggered an event for > n seconds. This prevents stale records from bloating the Map size.
    fn cleanup(&mut self, player: EcsEntity) {
        const TRACKING_TIMEOUT: u64 = 15;

        let now = Instant::now();
        self.event_history.retain(|entity, event| {
            now.duration_since(event.time) < Duration::from_secs(TRACKING_TIMEOUT)
                || entity.id() == player.id()
        });
    }

    /// When specific entity movements are detected, the associated sound (if any) needs to satisfy two conditions to
    /// be allowed to play:
    /// 1. An sfx.ron entry exists for the movement (we need to know which sound file(s) to play)
    /// 2. The sfx has not been played since it's timeout threshold has elapsed, which prevents firing every tick
    fn should_emit(
        last_play_entry: &LastSfxEvent,
        sfx_trigger_item: Option<(&SfxEvent, &SfxTriggerItem)>,
    ) -> bool {
        if let Some((event, item)) = sfx_trigger_item {
            if &last_play_entry.event == event {
                last_play_entry.time.elapsed().as_secs_f64() >= item.threshold
            } else {
                true
            }
        } else {
            false
        }
    }

    /// Voxygen has an existing list of character states via `MoveState::*` and `ActionState::*`
    /// however that list does not provide enough resolution to target specific entity events, such
    /// as opening or closing the glider. These methods translate those entity states with some additional
    /// data into more specific `SfxEvent`'s which we attach sounds to
    fn map_movement_event(current_event: &CharacterState, previous_event: SfxEvent) -> SfxEvent {
        match (current_event, previous_event) {
            (CharacterState::Roll(_), _) => SfxEvent::Roll,
            (CharacterState::Climb(_), _) => SfxEvent::Climb,
            (CharacterState::Idle(_), _) => SfxEvent::Run,
            (CharacterState::Idle(_), SfxEvent::Glide) => SfxEvent::GliderClose,
            (CharacterState::Idle(_), SfxEvent::Fall) => SfxEvent::Run,
            (CharacterState::Idle(_), SfxEvent::Jump) => SfxEvent::Idle,
            (CharacterState::Glide(_), previous_event) => {
                if previous_event != SfxEvent::GliderOpen && previous_event != SfxEvent::Glide {
                    SfxEvent::GliderOpen
                } else {
                    SfxEvent::Glide
                }
            }
            (CharacterState::Idle(_), SfxEvent::Glide) => SfxEvent::GliderClose,
            _ => SfxEvent::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{comp::CharacterState, event::SfxEvent};
    use std::time::{Duration, Instant};

    #[test]
    fn no_item_config_no_emit() {
        let last_sfx_event = LastSfxEvent {
            event: SfxEvent::Idle,
            time: Instant::now(),
        };

        let result = MovementEventMapper::should_emit(&last_sfx_event, None);

        assert_eq!(result, false);
    }

    #[test]
    fn config_but_played_since_threshold_no_emit() {
        let event = SfxEvent::Run;

        let trigger_item = SfxTriggerItem {
            files: vec![String::from("some.path.to.sfx.file")],
            threshold: 1.0,
        };

        // Triggered a 'Run' 0 seconds ago
        let last_sfx_event = LastSfxEvent {
            event: SfxEvent::Run,
            time: Instant::now(),
        };

        let result =
            MovementEventMapper::should_emit(&last_sfx_event, Some((&event, &trigger_item)));

        assert_eq!(result, false);
    }

    #[test]
    fn config_and_not_played_since_threshold_emits() {
        let event = SfxEvent::Run;

        let trigger_item = SfxTriggerItem {
            files: vec![String::from("some.path.to.sfx.file")],
            threshold: 0.5,
        };

        let last_sfx_event = LastSfxEvent {
            event: SfxEvent::Idle,
            time: Instant::now().checked_add(Duration::from_secs(1)).unwrap(),
        };

        let result =
            MovementEventMapper::should_emit(&last_sfx_event, Some((&event, &trigger_item)));

        assert_eq!(result, true);
    }

    #[test]
    fn same_previous_event_elapsed_emits() {
        let event = SfxEvent::Run;

        let trigger_item = SfxTriggerItem {
            files: vec![String::from("some.path.to.sfx.file")],
            threshold: 0.5,
        };

        let last_sfx_event = LastSfxEvent {
            event: SfxEvent::Run,
            time: Instant::now()
                .checked_sub(Duration::from_millis(500))
                .unwrap(),
        };

        let result =
            MovementEventMapper::should_emit(&last_sfx_event, Some((&event, &trigger_item)));

        assert_eq!(result, true);
    }

    #[test]
    fn maps_idle() {
        let result =
            MovementEventMapper::map_movement_event(&CharacterState::Idle(None), SfxEvent::Idle);
        assert_eq!(result, SfxEvent::Idle);
    }

    #[test]
    fn maps_roll() {
        let result =
            MovementEventMapper::map_movement_event(&CharacterState::Roll(None), SfxEvent::Run);
        assert_eq!(result, SfxEvent::Roll);
    }

    #[test]
    fn maps_land_on_ground_to_run() {
        let result =
            MovementEventMapper::map_movement_event(&CharacterState::Idle(None), SfxEvent::Fall);
        assert_eq!(result, SfxEvent::Run);
    }

    #[test]
    fn maps_glider_open() {
        let result =
            MovementEventMapper::map_movement_event(&CharacterState::Glide(None), SfxEvent::Jump);
        assert_eq!(result, SfxEvent::GliderOpen);
    }

    #[test]
    fn maps_glide() {
        let result =
            MovementEventMapper::map_movement_event(&CharacterState::Glide(None), SfxEvent::Glide);
        assert_eq!(result, SfxEvent::Glide);
    }

    #[test]
    fn maps_glider_close_when_closing_mid_flight() {
        let result =
            MovementEventMapper::map_movement_event(&CharacterState::Idle(None), SfxEvent::Glide);
        assert_eq!(result, SfxEvent::GliderClose);
    }

    #[test]
    fn maps_glider_close_when_landing() {
        let result =
            MovementEventMapper::map_movement_event(&CharacterState::Idle(None), SfxEvent::Glide);
        assert_eq!(result, SfxEvent::GliderClose);
    }
}
