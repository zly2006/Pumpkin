use core::f32;
use std::sync::{Arc, atomic::AtomicU32};

use async_trait::async_trait;
use pumpkin_data::{damage::DamageType, item::Item};
use pumpkin_protocol::{
    client::play::{CTakeItemEntity, MetaDataType, Metadata},
    codec::item_stack_serializer::ItemStackSerializer,
};
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::item::ItemStack;
use tokio::sync::Mutex;

use crate::server::Server;

use super::{Entity, EntityBase, living::LivingEntity, player::Player};

pub struct ItemEntity {
    entity: Entity,
    item_age: AtomicU32,
    // These cannot be atomic values because we mutate their state based on what they are; we run
    // into the ABA problem
    item_stack: Mutex<ItemStack>,
    pickup_delay: Mutex<u8>,
}

impl ItemEntity {
    pub async fn new(entity: Entity, item_id: u16, count: u32) -> Self {
        entity
            .set_velocity(Vector3::new(
                rand::random::<f64>() * 0.2 - 0.1,
                0.2,
                rand::random::<f64>() * 0.2 - 0.1,
            ))
            .await;
        entity.yaw.store(rand::random::<f32>() * 360.0);
        Self {
            entity,
            item_stack: Mutex::new(ItemStack::new(
                count as u8,
                Item::from_id(item_id).expect("We passed a bad item id into ItemEntity"),
            )),
            item_age: AtomicU32::new(0),
            pickup_delay: Mutex::new(10), // Vanilla pickup delay is 10 ticks
        }
    }
    pub async fn send_meta_packet(&self) {
        self.entity
            .send_meta_data(&[Metadata::new(
                8,
                MetaDataType::ItemStack,
                &ItemStackSerializer::from(self.item_stack.lock().await.clone()),
            )])
            .await;
    }
}

#[async_trait]
impl EntityBase for ItemEntity {
    async fn tick(&self, server: &Server) {
        self.entity.tick(server).await;
        {
            let mut delay = self.pickup_delay.lock().await;
            *delay = delay.saturating_sub(1);
        };

        let age = self
            .item_age
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if age >= 6000 {
            self.entity.remove().await;
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

    async fn on_player_collision(&self, player: Arc<Player>) {
        let can_pickup = {
            let delay = self.pickup_delay.lock().await;
            *delay == 0
        };

        if can_pickup {
            let mut inv = player.inventory.lock().await;
            let mut total_pick_up = 0;
            let mut slot_updates = Vec::new();
            let remove_entity = {
                let item_stack = self.item_stack.lock().await.clone();
                let mut stack_size = item_stack.item_count;
                let max_stack = item_stack.item.components.max_stack_size;
                while stack_size > 0 {
                    if let Some(slot) = inv.get_pickup_item_slot(item_stack.item.id) {
                        // Fill the inventory while there are items in the stack and space in the inventory
                        let maybe_stack = inv
                            .get_slot(slot)
                            .expect("collect item slot returned an invalid slot");

                        if let Some(existing_stack) = maybe_stack {
                            // We have the item in this stack already

                            // This is bounded to `u8::MAX`
                            let amount_to_fill = u32::from(max_stack - existing_stack.item_count);
                            // This is also bounded to `u8::MAX` since `amount_to_fill` is max `u8::MAX`
                            let amount_to_add = amount_to_fill.min(u32::from(stack_size));
                            // Therefore this is safe

                            // Update referenced stack so next call to `get_pickup_item_slot` is
                            // correct
                            existing_stack.item_count += amount_to_add as u8;
                            total_pick_up += amount_to_add;

                            debug_assert!(amount_to_add > 0);
                            stack_size = stack_size.saturating_sub(amount_to_add as u8);

                            slot_updates.push((slot, existing_stack.clone()));
                        } else {
                            // A new stack

                            // This is bounded to `u8::MAX`
                            let amount_to_fill = u32::from(max_stack);
                            // This is also bounded to `u8::MAX` since `amount_to_fill` is max `u8::MAX`
                            let amount_to_add = amount_to_fill.min(u32::from(stack_size));
                            total_pick_up += amount_to_add;

                            debug_assert!(amount_to_add > 0);
                            stack_size = stack_size.saturating_sub(amount_to_add as u8);

                            slot_updates.push((slot, self.item_stack.lock().await.clone()));
                        }
                    } else {
                        // We can't pick anything else up
                        break;
                    }
                }

                stack_size == 0
            };

            if total_pick_up > 0 {
                player
                    .client
                    .enqueue_packet(&CTakeItemEntity::new(
                        self.entity.entity_id.into(),
                        player.entity_id().into(),
                        total_pick_up.try_into().unwrap(),
                    ))
                    .await;
            }

            // TODO: Can we batch slot updates?
            for (slot, stack) in slot_updates {
                player.update_single_slot(&mut inv, slot, stack).await;
            }

            if remove_entity {
                self.entity.remove().await;
            } else {
                // Update entity
                self.send_meta_packet().await;
            }
        }
    }

    fn get_entity(&self) -> &Entity {
        &self.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        None
    }
}
