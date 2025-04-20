use std::sync::Arc;

use async_trait::async_trait;
use pumpkin_data::entity::EntityType;
use pumpkin_util::math::vector3::Vector3;
use tokio::sync::Mutex;
use zombie::Zombie;

use super::{
    Entity, EntityBase,
    ai::{goal::Goal, path::Navigator},
    living::LivingEntity,
};
use crate::entity::npc::NpcEntity;
use crate::{server::Server, world::World};

pub mod zombie;

pub struct MobEntity {
    pub living_entity: LivingEntity,
    pub goals: Mutex<Vec<(Arc<dyn Goal>, bool)>>,
    pub navigator: Mutex<Navigator>,
}

#[async_trait]
impl EntityBase for MobEntity {
    async fn tick(&self, server: &Server) {
        self.living_entity.tick(server).await;
        let mut goals = self.goals.lock().await;
        for (goal, running) in goals.iter_mut() {
            if *running {
                if goal.should_continue(self).await {
                    goal.tick(self).await;
                } else {
                    *running = false;
                }
            } else {
                *running = goal.can_start(self).await;
            }
        }
        let mut navigator = self.navigator.lock().await;
        navigator.tick(&self.living_entity).await;
    }

    fn get_entity(&self) -> &Entity {
        &self.living_entity.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(&self.living_entity)
    }
}

pub async fn from_type(
    entity_type: EntityType,
    position: Vector3<f64>,
    world: &Arc<World>,
) -> Arc<dyn EntityBase> {
    let entity = world.create_entity(position, entity_type);
    let mob = MobEntity {
        living_entity: LivingEntity::new(entity),
        goals: Mutex::new(vec![]),
        navigator: Mutex::new(Navigator::default()),
    };
    match entity_type {
        EntityType::ZOMBIE => {
            Zombie::make(&mob).await;
            Arc::new(mob)
        }
        EntityType::NPC => Arc::new(NpcEntity::new(mob)),
        // TODO
        _ => Arc::new(mob),
    }
}

impl MobEntity {
    pub async fn goal<T: Goal + 'static>(&self, goal: T) {
        self.goals.lock().await.push((Arc::new(goal), false));
    }
}
