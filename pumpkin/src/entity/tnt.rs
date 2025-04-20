use crate::server::Server;
use async_trait::async_trait;
use core::f32;
use pumpkin_data::{block::Block, damage::DamageType};
use pumpkin_protocol::{
    client::play::{MetaDataType, Metadata},
    codec::var_int::VarInt,
};
use pumpkin_util::math::vector3::Vector3;
use std::sync::atomic::AtomicU32;

use super::{Entity, EntityBase, living::LivingEntity};

pub struct TNTEntity {
    entity: Entity,
    power: f32,
    fuse: AtomicU32,
}

impl TNTEntity {
    pub fn new(entity: Entity, power: f32, fuse: u32) -> Self {
        Self {
            entity,
            power,
            fuse: AtomicU32::new(fuse),
        }
    }
    pub async fn send_meta_packet(&self) {
        // TODO: Yes, this is the wrong function, but we need to send this after spawning the entity.
        let pos: f64 = rand::random::<f64>() * 6.283_185_482_025_146_5;
        self.entity
            .set_velocity(Vector3::new(-pos.sin() * 0.02, 0.2, -pos.cos() * 0.02))
            .await;
        // We can merge multiple `Metadata`s into one meta packet.
        self.entity
            .send_meta_data(&[
                Metadata::new(
                    8,
                    MetaDataType::Integer,
                    VarInt(self.fuse.load(std::sync::atomic::Ordering::Relaxed) as i32),
                ),
                Metadata::new(
                    9,
                    MetaDataType::BlockState,
                    VarInt(i32::from(Block::TNT.default_state_id)),
                ),
            ])
            .await;
    }
}

#[async_trait]
impl EntityBase for TNTEntity {
    async fn tick(&self, server: &Server) {
        let fuse = self.fuse.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        if fuse == 0 {
            self.entity.remove().await;
            self.entity
                .world
                .read()
                .await
                .explode(server, self.entity.pos.load(), self.power)
                .await;
        }
    }
    async fn damage(
        &self,
        amount: f32,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
    ) -> bool {
        false
    }

    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }
}
