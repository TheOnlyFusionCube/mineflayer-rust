//! `bot.getExplosionDamages`: the exposure raycast and the armor and
//! difficulty scaling of upstream's formula.

use super::*;

/// Upstream `getAttributeValue` — fold an attribute snapshot's base and
/// modifiers: op0 adds to base, op1 scales by base, op2 compounds.
pub(super) fn attribute_value(snapshot: &AttributeSnapshot) -> f64 {
    use mineflayer_engine::core::attribute_modifier_operation::AttributeModifierOperation::*;
    let mut base = snapshot.base;
    for m in &snapshot.modifiers {
        if m.operation == AddValue {
            base += m.amount;
        }
    }
    let mut total = base;
    for m in &snapshot.modifiers {
        if m.operation == AddMultipliedBase {
            total += base * m.amount;
        }
    }
    for m in &snapshot.modifiers {
        if m.operation == AddMultipliedTotal {
            total += total * m.amount;
        }
    }
    total
}

/// Upstream `getDamageAfterAbsorb` (armor damage reduction).
pub(super) fn damage_after_absorb(damages: f64, armor: f64, toughness: f64) -> f64 {
    let factor = 2.0 + toughness / 4.0;
    let effective = (armor - damages / factor).max(armor * 0.2).min(20.0);
    damages * (1.0 - effective / 25.0)
}

impl Bot {
    /// `bot.getExplosionDamages(entity, sourcePos, power, rawDamages?)` —
    /// estimate the damage an explosion of `power` at `source_pos` would deal
    /// to `entity`. Port of upstream's formula: exposure is raycast against
    /// loaded chunks; armor comes from attributes tracked off
    /// `c_update_attributes` (the engine doesn't store armor attributes).
    /// Returns `None` when the entity's position or armor data is unknown
    /// (upstream: `null`).
    pub fn get_explosion_damages(
        &self,
        entity: &EntityRef,
        source_pos: Vec3,
        power: f64,
        raw: bool,
    ) -> Option<f32> {
        const DAMAGE_MULTIPLIER: f64 = 7.0;
        let pos = entity.get_component::<Position>().map(|p| **p)?;
        let distance = pos.distance_to(source_pos);
        let radius = 2.0 * power;
        if distance >= radius {
            return Some(0.0);
        }
        let exposure = self.explosion_exposure(pos, source_pos);
        let impact = (1.0 - distance / radius) * exposure;
        let mut damages = ((impact * impact + impact) * DAMAGE_MULTIPLIER * power + 1.0).floor();

        if !raw {
            let id = *entity.get_component::<MinecraftEntityId>()?;
            let attributes = self.shared.entities.read().attributes.get(&id).cloned();
            let armor = attributes
                .as_ref()
                .and_then(|a| a.get(&Attribute::Armor))
                .map(attribute_value)?;
            let toughness = attributes
                .as_ref()
                .and_then(|a| a.get(&Attribute::ArmorToughness))
                .map(attribute_value)
                .unwrap_or(0.0);
            damages = damage_after_absorb(damages, armor, toughness);
            if entity
                .get_component::<EntityKindComponent>()
                .map(|kind| kind.0)
                == Some(EntityKind::Player)
            {
                let scale = match self
                    .shared
                    .entities
                    .read()
                    .difficulty
                    .unwrap_or(Difficulty::Normal)
                {
                    Difficulty::Peaceful => 0.0,
                    Difficulty::Easy => 0.5,
                    Difficulty::Normal => 1.0,
                    Difficulty::Hard => 1.5,
                };
                damages *= scale;
            }
        }
        Some(damages.floor() as f32)
    }

    /// Upstream `calcExposure` — sample a grid of points over the entity's
    /// 0.6×1.8 bounding box and count how many have line of sight to the
    /// explosion.
    pub(super) fn explosion_exposure(&self, player_pos: Vec3, explosion_pos: Vec3) -> f64 {
        let dx: f64 = 1.0 / (0.6 * 2.0 + 1.0);
        let dy: f64 = 1.0 / (1.8 * 2.0 + 1.0);
        let dz: f64 = 1.0 / (0.6 * 2.0 + 1.0);
        let d3 = (1.0 - (1.0 / dx).floor() * dx) / 2.0;
        let d4 = (1.0 - (1.0 / dz).floor() * dz) / 2.0;

        let world = self.client.world();
        let world = world.read();
        let mut sampled = 0u32;
        let mut exposed = 0u32;
        let mut y = player_pos.y;
        while y <= player_pos.y + 1.8 {
            let mut x = player_pos.x - 0.3 + d3;
            while x <= player_pos.x + 0.3 {
                let mut z = player_pos.z - 0.3 + d4;
                while z <= player_pos.z + 0.3 {
                    let hit = clip(
                        &world.chunks,
                        ClipContext {
                            from: explosion_pos,
                            to: Vec3::new(x, y, z),
                            block_shape_type: BlockShapeType::Collider,
                            fluid_pick_type: FluidPickType::None,
                        },
                    );
                    if hit.miss {
                        exposed += 1;
                    }
                    sampled += 1;
                    z += 0.6 * dz;
                }
                x += 0.6 * dx;
            }
            y += 1.8 * dy;
        }
        exposed as f64 / sampled.max(1) as f64
    }
}

#[cfg(test)]
mod tests {
    use mineflayer_engine::{
        core::attribute_modifier_operation::AttributeModifierOperation,
        inventory::components::AttributeModifier,
    };

    use super::*;

    fn snapshot(base: f64, modifiers: Vec<AttributeModifier>) -> AttributeSnapshot {
        AttributeSnapshot {
            attribute: Attribute::Armor,
            base,
            modifiers,
        }
    }

    fn modifier(amount: f64, operation: AttributeModifierOperation) -> AttributeModifier {
        AttributeModifier {
            id: mineflayer_engine::Identifier::new("test"),
            amount,
            operation,
        }
    }

    #[test]
    fn attribute_value_folds_modifiers_like_upstream() {
        // no modifiers -> base
        assert_eq!(attribute_value(&snapshot(20.0, vec![])), 20.0);
        // op0 (add value): base + amount
        assert_eq!(
            attribute_value(&snapshot(
                20.0,
                vec![modifier(2.0, AttributeModifierOperation::AddValue)]
            )),
            22.0
        );
        // op1 (add multiplied base): base + base*amount
        assert_eq!(
            attribute_value(&snapshot(
                20.0,
                vec![modifier(0.5, AttributeModifierOperation::AddMultipliedBase)]
            )),
            30.0
        );
        // op2 (add multiplied total): total + total*amount
        assert_eq!(
            attribute_value(&snapshot(
                20.0,
                vec![modifier(
                    0.5,
                    AttributeModifierOperation::AddMultipliedTotal
                )]
            )),
            30.0
        );
        // combined: (20 + 2) + 22*0.5 then *2 = 66
        assert_eq!(
            attribute_value(&snapshot(
                20.0,
                vec![
                    modifier(2.0, AttributeModifierOperation::AddValue),
                    modifier(0.5, AttributeModifierOperation::AddMultipliedBase),
                    modifier(1.0, AttributeModifierOperation::AddMultipliedTotal),
                ]
            )),
            66.0
        );
    }

    #[test]
    fn armor_absorb_matches_upstream_formula() {
        // iron armor: 15 armor, 0 toughness vs 15 damage ->
        // effective = min(max(15 - 15/2, 3), 20) = 7.5; 15 * (1 - 7.5/25) = 10.5
        assert_eq!(damage_after_absorb(15.0, 15.0, 0.0), 10.5);
        // diamond: 20 armor, 8 toughness vs 15 -> factor 4; eff = min(max(20-3.75,4),20)=16.25; 15*(1-.65)=5.25
        assert_eq!(damage_after_absorb(15.0, 20.0, 8.0), 5.25);
        // armor floor: armor*0.2 when the reduction term goes below it
        assert_eq!(damage_after_absorb(100.0, 10.0, 0.0), 92.0);
    }
}
