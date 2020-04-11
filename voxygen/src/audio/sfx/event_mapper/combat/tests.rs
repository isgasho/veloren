use super::*;
use common::{
    assets,
    comp::{
        item::tool::{AxeKind, BowKind, SwordKind, ToolCategory, ToolKind},
        CharacterAbilityType, CharacterState, ItemConfig, Loadout,
    },
    event::SfxEvent,
    states,
};
use std::time::{Duration, Instant};

#[test]
fn maps_wield_while_equipping() {
    let mut loadout = Loadout::default();

    loadout.active_item = Some(ItemConfig {
        item: assets::load_expect_cloned("common.items.weapons.starter_axe"),
        ability1: None,
        ability2: None,
        ability3: None,
        block_ability: None,
        dodge_ability: None,
    });

    let result = CombatEventMapper::map_event(
        &CharacterState::Equipping(states::equipping::Data {
            time_left: Duration::from_millis(10),
        }),
        &PreviousEntityState {
            event: SfxEvent::Idle,
            time: Instant::now(),
            weapon_drawn: false,
        },
        Some(&loadout),
    );

    assert_eq!(result, SfxEvent::Wield(ToolCategory::Axe));
}

#[test]
fn maps_unwield() {
    let mut loadout = Loadout::default();

    loadout.active_item = Some(ItemConfig {
        item: assets::load_expect_cloned("common.items.weapons.starter_bow"),
        ability1: None,
        ability2: None,
        ability3: None,
        block_ability: None,
        dodge_ability: None,
    });

    let result = CombatEventMapper::map_event(
        &CharacterState::default(),
        &PreviousEntityState {
            event: SfxEvent::Idle,
            time: Instant::now(),
            weapon_drawn: true,
        },
        Some(&loadout),
    );

    assert_eq!(result, SfxEvent::Unwield(ToolCategory::Bow));
}

#[test]
fn maps_basic_melee() {
    let mut loadout = Loadout::default();

    loadout.active_item = Some(ItemConfig {
        item: assets::load_expect_cloned("common.items.weapons.starter_axe"),
        ability1: None,
        ability2: None,
        ability3: None,
        block_ability: None,
        dodge_ability: None,
    });

    let result = CombatEventMapper::map_event(
        &CharacterState::BasicMelee(states::basic_melee::Data {
            buildup_duration: Duration::default(),
            recover_duration: Duration::default(),
            base_healthchange: 1,
            range: 1.0,
            max_angle: 1.0,
            exhausted: false,
        }),
        &PreviousEntityState {
            event: SfxEvent::Idle,
            time: Instant::now(),
            weapon_drawn: true,
        },
        Some(&loadout),
    );

    assert_eq!(
        result,
        SfxEvent::Attack(CharacterAbilityType::BasicMelee, ToolCategory::Axe)
    );
}

#[test]
fn maps_triple_strike() {
    let mut loadout = Loadout::default();

    loadout.active_item = Some(ItemConfig {
        item: assets::load_expect_cloned("common.items.weapons.starter_sword"),
        ability1: None,
        ability2: None,
        ability3: None,
        block_ability: None,
        dodge_ability: None,
    });

    let result = CombatEventMapper::map_event(
        &CharacterState::TripleStrike(states::triple_strike::Data {
            base_damage: 10,
            stage: states::triple_strike::Stage::First,
            stage_time_active: Duration::default(),
            stage_exhausted: false,
            initialized: true,
            transition_style: states::triple_strike::TransitionStyle::Hold(
                states::triple_strike::HoldingState::Released,
            ),
        }),
        &PreviousEntityState {
            event: SfxEvent::Idle,
            time: Instant::now(),
            weapon_drawn: true,
        },
        Some(&loadout),
    );

    assert_eq!(
        result,
        SfxEvent::Attack(CharacterAbilityType::TripleStrike, ToolCategory::Sword)
    );
}
