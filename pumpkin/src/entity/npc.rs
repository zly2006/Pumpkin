use crate::entity::living::LivingEntity;
use crate::entity::mob::MobEntity;
use crate::entity::{Entity, EntityBase};

pub struct NpcEntity {
    pub mob_entity: MobEntity,
    pub name: String,
}

impl NpcEntity {
}

impl NpcEntity {
    pub async fn new(mob: MobEntity) -> Self {
        Self {
            mob_entity: mob,
            name: String::new(),
        }
    }
}

impl EntityBase for NpcEntity {
    fn get_entity(&self) -> &Entity {
        &self.mob_entity.living_entity.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(&self.mob_entity.living_entity)
    }
}
