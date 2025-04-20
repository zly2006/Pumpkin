use crate::entity::living::LivingEntity;
use crate::entity::mob::MobEntity;
use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase};
use crate::server::Server;
use async_trait::async_trait;
use core::f32;
use pumpkin_data::damage::DamageType;
use pumpkin_util::text::TextComponent;

pub struct NpcEntity {
    pub mob_entity: MobEntity,
    pub name: String,
}

impl NpcEntity {
    pub async fn new(mob: MobEntity) -> Self {
        Self {
            mob_entity: mob,
            name: String::from(format!("NPC {}", rand::random::<u8>())),
        }
    }
}

#[async_trait]
impl EntityBase for NpcEntity {
    async fn tick(&self, server: &Server) {
        self.mob_entity.tick(server).await;
        // look at closest player
        let mut closest_distance = f64::MAX;
        let mut closest_player = None;
        for player in self
            .get_entity()
            .world
            .read()
            .await
            .players
            .read()
            .await
            .values()
        {
            let distance = self
                .get_entity()
                .pos
                .load()
                .squared_distance_to_vec(player.position());
            if distance < closest_distance {
                closest_distance = distance;
                closest_player = Some(player.clone());
            }
        }
        if let Some(player) = closest_player {
            self.get_entity().look_at(player.position()).await;
        }
    }
    async fn damage(
        &self,
        amount: f32,
        damage_type: DamageType,
        source: Option<&dyn EntityBase>,
    ) -> bool {
        if let Some(entity) = source {
            if let Some(player) = entity.downcast::<Player>() {
                player
                    .send_system_message(&TextComponent::text(format!(
                        "You hit the NPC! {}",
                        self.name
                    )))
                    .await;
            }
        }
        self.mob_entity.damage(amount, damage_type, source).await
    }

    fn get_entity(&self) -> &Entity {
        &self.mob_entity.living_entity.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(&self.mob_entity.living_entity)
    }
}
