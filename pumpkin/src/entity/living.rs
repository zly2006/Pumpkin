use core::f32;
use std::sync::atomic::AtomicU8;
use std::{collections::HashMap, sync::atomic::AtomicI32};

use crate::server::Server;
use async_trait::async_trait;
use crossbeam::atomic::AtomicCell;
use pumpkin_config::advanced_config;
use pumpkin_data::entity::{EffectType, EntityStatus};
use pumpkin_data::{damage::DamageType, sound::Sound};
use pumpkin_nbt::tag::NbtTag;
use pumpkin_protocol::client::play::{CHurtAnimation, CTakeItemEntity};
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::{
    client::play::{CDamageEvent, CSetEquipment, EquipmentSlot, MetaDataType, Metadata},
    codec::item_stack_serializer::ItemStackSerializer,
};
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::item::ItemStack;
use tokio::sync::Mutex;

use super::EntityBase;
use super::{Entity, EntityId, NBTStorage, effect::Effect};

/// Represents a living entity within the game world.
///
/// This struct encapsulates the core properties and behaviors of living entities, including players, mobs, and other creatures.
pub struct LivingEntity {
    /// The underlying entity object, providing basic entity information and functionality.
    pub entity: Entity,
    /// The last known position of the entity.
    pub last_pos: AtomicCell<Vector3<f64>>,
    /// Tracks the remaining time until the entity can regenerate health.
    pub time_until_regen: AtomicI32,
    /// Stores the amount of damage the entity last received.
    pub last_damage_taken: AtomicCell<f32>,
    /// The current health level of the entity.
    pub health: AtomicCell<f32>,
    pub death_time: AtomicU8,
    /// The distance the entity has been falling.
    pub fall_distance: AtomicCell<f32>,
    pub active_effects: Mutex<HashMap<EffectType, Effect>>,
}
impl LivingEntity {
    pub fn new(entity: Entity) -> Self {
        Self {
            entity,
            last_pos: AtomicCell::new(Vector3::new(0.0, 0.0, 0.0)),
            time_until_regen: AtomicI32::new(0),
            last_damage_taken: AtomicCell::new(0.0),
            health: AtomicCell::new(20.0),
            fall_distance: AtomicCell::new(0.0),
            death_time: AtomicU8::new(0),
            active_effects: Mutex::new(HashMap::new()),
        }
    }

    pub async fn send_equipment_changes(&self, equipment: &[(EquipmentSlot, ItemStack)]) {
        let equipment: Vec<(EquipmentSlot, ItemStackSerializer)> = equipment
            .iter()
            .map(|(slot, stack)| (*slot, ItemStackSerializer::from(stack.clone())))
            .collect();
        self.entity
            .world
            .read()
            .await
            .broadcast_packet_except(
                &[self.entity.entity_uuid],
                &CSetEquipment::new(self.entity_id().into(), equipment),
            )
            .await;
    }

    /// Picks up and Item entity or XP Orb
    pub async fn pickup(&self, item: &Entity, stack_amount: u32) {
        // TODO: Only nearby
        self.entity
            .world
            .read()
            .await
            .broadcast_packet_all(&CTakeItemEntity::new(
                item.entity_id.into(),
                self.entity.entity_id.into(),
                stack_amount.try_into().unwrap(),
            ))
            .await;
    }

    pub fn set_pos(&self, position: Vector3<f64>) {
        self.last_pos.store(self.entity.pos.load());
        self.entity.set_pos(position);
    }

    pub async fn heal(&self, additional_health: f32) {
        assert!(additional_health > 0.0);
        self.set_health(self.health.load() + additional_health)
            .await;
    }

    pub async fn set_health(&self, health: f32) {
        self.health.store(health);
        // tell everyone entities health changed
        self.entity
            .send_meta_data(&[Metadata::new(9, MetaDataType::Float, health)])
            .await;
    }

    pub const fn entity_id(&self) -> EntityId {
        self.entity.entity_id
    }

    pub async fn damage_with_context(
        &self,
        amount: f32,
        damage_type: DamageType,
        position: Option<Vector3<f64>>,
        source: Option<&Entity>,
        cause: Option<&Entity>,
    ) -> bool {
        // Check invulnerability before applying damage
        if self.entity.is_invulnerable_to(&damage_type) {
            return false;
        }

        self.entity
            .world
            .read()
            .await
            .broadcast_packet_all(&CDamageEvent::new(
                self.entity.entity_id.into(),
                damage_type.id.into(),
                source.map(|e| e.entity_id.into()),
                cause.map(|e| e.entity_id.into()),
                position,
            ))
            .await;

        let new_health = (self.health.load() - amount).max(0.0);

        if new_health == 0.0 {
            self.kill().await;
        } else {
            self.set_health(new_health).await;
        }

        true
    }

    pub async fn add_effect(&self, effect: Effect) {
        let mut effects = self.active_effects.lock().await;
        effects.insert(effect.r#type, effect);
        // TODO broadcast metadata
    }

    pub async fn has_effect(&self, effect: EffectType) -> bool {
        let effects = self.active_effects.lock().await;
        effects.contains_key(&effect)
    }

    pub async fn get_effect(&self, effect: EffectType) -> Option<Effect> {
        let effects = self.active_effects.lock().await;
        effects.get(&effect).cloned()
    }

    /// Returns if the entity was damaged or not
    pub fn check_damage(&self, amount: f32) -> bool {
        let regen = self
            .time_until_regen
            .load(std::sync::atomic::Ordering::Relaxed);

        let last_damage = self.last_damage_taken.load();
        // TODO: check if bypasses iframe
        if regen > 10 {
            if amount <= last_damage {
                return false;
            }
        } else {
            self.time_until_regen
                .store(20, std::sync::atomic::Ordering::Relaxed);
        }

        self.last_damage_taken.store(amount);
        amount > 0.0
    }

    pub async fn update_fall_distance(
        &self,
        height_difference: f64,
        ground: bool,
        dont_damage: bool,
    ) {
        if ground {
            let fall_distance = self.fall_distance.swap(0.0);
            if fall_distance <= 0.0 || dont_damage {
                return;
            }

            let safe_fall_distance = 3.0;
            let mut damage = fall_distance - safe_fall_distance;
            damage = (damage).ceil();

            // TODO: Play block fall sound
            let check_damage = self.damage(damage, DamageType::FALL, None).await; // Fall
            if check_damage {
                self.entity
                    .play_sound(Self::get_fall_sound(fall_distance as i32))
                    .await;
            }
        } else if height_difference < 0.0 {
            let distance = self.fall_distance.load();
            self.fall_distance
                .store(distance - (height_difference as f32));
        }
    }

    fn get_fall_sound(distance: i32) -> Sound {
        if distance > 4 {
            Sound::EntityGenericBigFall
        } else {
            Sound::EntityGenericSmallFall
        }
    }

    /// Kills the Entity
    ///
    /// This is similar to `kill` but Spawn Particles, Animation and plays death sound
    pub async fn kill(&self) {
        self.set_health(0.0).await;

        // Plays the death sound
        self.entity
            .world
            .read()
            .await
            .send_entity_status(
                &self.entity,
                EntityStatus::PlayDeathSoundOrAddProjectileHitParticles,
            )
            .await;
    }
}

#[async_trait]
impl EntityBase for LivingEntity {
    async fn tick(&self, server: &Server) {
        self.entity.tick(server).await;

        if self
            .time_until_regen
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
        {
            self.time_until_regen
                .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        if self.health.load() <= 0.0 {
            let time = self
                .death_time
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if time >= 20 {
                // Spawn Death particles
                self.entity
                    .world
                    .read()
                    .await
                    .send_entity_status(&self.entity, EntityStatus::AddDeathParticles)
                    .await;
                self.entity.remove().await;
            }
        }
    }
    async fn damage(
        &self,
        amount: f32,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
    ) -> bool {
        let world = self.entity.world.read().await;
        if !self.check_damage(amount) {
            return false;
        }
        let config = &advanced_config().pvp;

        if !self
            .damage_with_context(
                amount,
                damage_type,
                None,
                source.map(|e| e.get_entity()),
                source.map(|e| e.get_entity()),
            )
            .await
        {
            return false;
        }

        if config.hurt_animation {
            let entity_id = VarInt(self.entity.entity_id);
            world
                .broadcast_packet_all(&CHurtAnimation::new(entity_id, self.entity.yaw.load()))
                .await;
        }
        true
    }
    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(self)
    }
}

#[async_trait]
impl NBTStorage for LivingEntity {
    async fn write_nbt(&self, nbt: &mut pumpkin_nbt::compound::NbtCompound) {
        self.entity.write_nbt(nbt).await;
        nbt.put("Health", NbtTag::Float(self.health.load()));
        // todo more...
    }

    async fn read_nbt(&mut self, nbt: &mut pumpkin_nbt::compound::NbtCompound) {
        self.entity.read_nbt(nbt).await;
        self.health.store(nbt.get_float("Health").unwrap_or(0.0));
        // todo more...
    }
}
