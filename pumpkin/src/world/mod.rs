use std::{
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
};

pub mod chunker;
pub mod explosion;
pub mod time;

use crate::{
    PLUGIN_MANAGER,
    block::{self, registry::BlockRegistry},
    command::client_suggestions,
    entity::{Entity, EntityBase, EntityId, player::Player},
    error::PumpkinError,
    plugin::{
        block::block_break::BlockBreakEvent,
        player::{player_join::PlayerJoinEvent, player_leave::PlayerLeaveEvent},
        world::{chunk_load::ChunkLoad, chunk_save::ChunkSave, chunk_send::ChunkSend},
    },
    server::Server,
};
use bitflags::bitflags;
use border::Worldborder;
use bytes::Bytes;
use explosion::Explosion;
use pumpkin_config::BasicConfiguration;
use pumpkin_data::{
    block::{Block, get_block_and_state_by_state_id, get_block_by_state_id, get_state_by_state_id},
    entity::{EntityStatus, EntityType},
    fluid::Fluid,
    particle::Particle,
    sound::{Sound, SoundCategory},
    world::{RAW, WorldEvent},
};
use pumpkin_macros::send_cancellable;
use pumpkin_nbt::to_bytes_unnamed;
use pumpkin_protocol::client::play::PlayerAction::AddPlayer;
use pumpkin_protocol::{
    ClientPacket, IdOr, SoundEvent,
    client::play::{
        CBlockEntityData, CEntityStatus, CGameEvent, CLogin, CMultiBlockUpdate, CPlayerChatMessage,
        CPlayerInfoUpdate, CRemoveEntities, CRemovePlayerInfo, CSoundEffect, CSpawnEntity,
        FilterType, GameEvent, InitChat, PlayerAction, PlayerInfoFlags,
    },
    server::play::SChatMessage,
};
use pumpkin_protocol::{client::play::CLevelEvent, codec::identifier::Identifier};
use pumpkin_protocol::{
    client::play::{
        CBlockUpdate, CDisguisedChatMessage, CExplosion, CRespawn, CSetBlockDestroyStage,
        CWorldEvent,
    },
    codec::var_int::VarInt,
};
use pumpkin_registry::DimensionType;
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_util::math::{position::chunk_section_from_pos, vector2::Vector2};
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_world::{
    BlockStateId, GENERATION_SETTINGS, GeneratorSetting, biome, block::entities::BlockEntity,
    level::SyncChunk,
};
use pumpkin_world::{block::BlockDirection, chunk::ChunkData};
use pumpkin_world::{chunk::TickPriority, level::Level};
use rand::{Rng, thread_rng};
use scoreboard::Scoreboard;
use thiserror::Error;
use time::LevelTime;
use tokio::sync::{RwLock, mpsc};
use tokio::{
    select,
    sync::{Mutex, mpsc::UnboundedReceiver},
};

pub mod border;
pub mod bossbar;
pub mod custom_bossbar;
pub mod scoreboard;
pub mod weather;

use weather::Weather;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct BlockFlags: u32 {
        const NOTIFY_NEIGHBORS                      = 0b000_0000_0001;
        const NOTIFY_LISTENERS                      = 0b000_0000_0010;
        const NOTIFY_ALL                            = 0b000_0000_0011;
        const FORCE_STATE                           = 0b000_0000_0100;
        const SKIP_DROPS                            = 0b000_0000_1000;
        const MOVED                                 = 0b000_0001_0000;
        const SKIP_REDSTONE_WIRE_STATE_REPLACEMENT  = 0b000_0010_0000;
        const SKIP_BLOCK_ENTITY_REPLACED_CALLBACK   = 0b000_0100_0000;
        const SKIP_BLOCK_ADDED_CALLBACK             = 0b000_1000_0000;
    }
}

#[derive(Debug, Error)]
pub enum GetBlockError {
    InvalidBlockId,
    BlockOutOfWorldBounds,
}

impl std::fmt::Display for GetBlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl PumpkinError for GetBlockError {
    fn is_kick(&self) -> bool {
        false
    }

    fn severity(&self) -> log::Level {
        log::Level::Warn
    }

    fn client_kick_reason(&self) -> Option<String> {
        None
    }
}

/// Represents a Minecraft world, containing entities, players, and the underlying level data.
///
/// Each dimension (Overworld, Nether, End) typically has its own `World`.
///
/// **Key Responsibilities:**
///
/// - Manages the `Level` instance for handling chunk-related operations.
/// - Stores and tracks active `Player` entities within the world.
/// - Provides a central hub for interacting with the world's entities and environment.
pub struct World {
    /// The underlying level, responsible for chunk management and terrain generation.
    pub level: Arc<Level>,
    /// A map of active players within the world, keyed by their unique UUID.
    pub players: Arc<RwLock<HashMap<uuid::Uuid, Arc<Player>>>>,
    /// A map of active entities within the world, keyed by their unique UUID.
    /// This does not include players.
    pub entities: Arc<RwLock<HashMap<uuid::Uuid, Arc<dyn EntityBase>>>>,
    /// The world's scoreboard, used for tracking scores, objectives, and display information.
    pub scoreboard: Mutex<Scoreboard>,
    /// The world's worldborder, defining the playable area and controlling its expansion or contraction.
    pub worldborder: Mutex<Worldborder>,
    /// The world's time, including counting ticks for weather, time cycles, and statistics.
    pub level_time: Mutex<LevelTime>,
    /// The type of dimension the world is in.
    pub dimension_type: DimensionType,
    pub sea_level: i32,
    /// The world's weather, including rain and thunder levels.
    pub weather: Mutex<Weather>,
    /// Block Behaviour
    pub block_registry: Arc<BlockRegistry>,
    /// A map of unsent block changes, keyed by block position.
    unsent_block_changes: Mutex<HashMap<BlockPos, u16>>,
    // TODO: entities
}

impl World {
    #[must_use]
    pub fn load(
        level: Level,
        dimension_type: DimensionType,
        block_registry: Arc<BlockRegistry>,
    ) -> Self {
        // TODO
        let generation_settings = GENERATION_SETTINGS
            .get(&GeneratorSetting::Overworld)
            .unwrap();
        Self {
            level: Arc::new(level),
            players: Arc::new(RwLock::new(HashMap::new())),
            entities: Arc::new(RwLock::new(HashMap::new())),
            scoreboard: Mutex::new(Scoreboard::new()),
            worldborder: Mutex::new(Worldborder::new(0.0, 0.0, 29_999_984.0, 0, 0, 0)),
            level_time: Mutex::new(LevelTime::new()),
            dimension_type,
            weather: Mutex::new(Weather::new()),
            block_registry,
            sea_level: generation_settings.sea_level,
            unsent_block_changes: Mutex::new(HashMap::new()),
        }
    }

    pub async fn shutdown(&self) {
        self.level.shutdown().await;
    }

    pub async fn send_entity_status(&self, entity: &Entity, status: EntityStatus) {
        // TODO: only nearby
        self.broadcast_packet_all(&CEntityStatus::new(entity.entity_id, status as i8))
            .await;
    }

    /// Broadcasts a packet to all connected players within the world.
    ///
    /// Sends the specified packet to every player currently logged in to the world.
    ///
    /// **Note:** This function acquires a lock on the `current_players` map, ensuring thread safety.
    pub async fn broadcast_packet_all<P>(&self, packet: &P)
    where
        P: ClientPacket,
    {
        self.broadcast_packet_except(&[], packet).await;
    }

    pub async fn broadcast_message(
        &self,
        message: &TextComponent,
        sender_name: &TextComponent,
        chat_type: u8,
        target_name: Option<&TextComponent>,
    ) {
        self.broadcast_packet_all(&CDisguisedChatMessage::new(
            message,
            (chat_type + 1).into(),
            sender_name,
            target_name,
        ))
        .await;
    }

    pub async fn broadcast_secure_player_chat(
        &self,
        sender: &Arc<Player>,
        chat_message: &SChatMessage,
        decorated_message: &TextComponent,
    ) {
        let messages_sent: i32 = sender.chat_session.lock().await.messages_sent;
        let sender_last_seen = {
            let cache = sender.signature_cache.lock().await;
            cache.last_seen.clone()
        };

        let current_players = self.players.read().await;
        for (_, recipient) in current_players.iter() {
            let messages_received: i32 = recipient.chat_session.lock().await.messages_received;
            let packet = &CPlayerChatMessage::new(
                VarInt(messages_received),
                sender.gameprofile.id,
                VarInt(messages_sent),
                chat_message.signature.clone(),
                chat_message.message.clone(),
                chat_message.timestamp,
                chat_message.salt,
                sender_last_seen.indexed_for(recipient).await,
                Some(decorated_message.clone()),
                FilterType::PassThrough,
                (RAW + 1).into(), // Custom registry chat_type with no sender name
                TextComponent::text(""), // Not needed since we're injecting the name in the message for custom formatting
                None,
            );
            recipient.client.enqueue_packet(packet).await;

            recipient
                .signature_cache
                .lock()
                .await
                .add_seen_signature(&chat_message.signature.clone().unwrap()); // Unwrap is safe because we check for None in validate_chat_message

            let recipient_signature_cache = &mut recipient.signature_cache.lock().await;
            if recipient.gameprofile.id != sender.gameprofile.id {
                // Sender may update recipient on signatures recipient hasn't seen
                recipient_signature_cache.cache_signatures(sender_last_seen.as_ref());
            }
            recipient.chat_session.lock().await.messages_received += 1;
        }

        sender.chat_session.lock().await.messages_sent += 1;
    }

    /// Broadcasts a packet to all connected players within the world, excluding the specified players.
    ///
    /// Sends the specified packet to every player currently logged in to the world, excluding the players listed in the `except` parameter.
    ///
    /// **Note:** This function acquires a lock on the `current_players` map, ensuring thread safety.
    pub async fn broadcast_packet_except<P>(&self, except: &[uuid::Uuid], packet: &P)
    where
        P: ClientPacket,
    {
        let mut packet_buf = Vec::new();
        if let Err(err) = packet.write(&mut packet_buf) {
            log::error!("Failed to serialize packet {}: {}", P::PACKET_ID, err);
            return;
        }
        let packet_data: Bytes = packet_buf.into();

        let current_players = self.players.read().await;
        for (_, player) in current_players.iter().filter(|c| !except.contains(c.0)) {
            player.client.enqueue_packet_data(packet_data.clone()).await;
        }
    }

    pub async fn spawn_particle(
        &self,
        position: Vector3<f64>,
        offset: Vector3<f32>,
        max_speed: f32,
        particle_count: i32,
        particle: Particle,
    ) {
        let players = self.players.read().await;
        for (_, player) in players.iter() {
            player
                .spawn_particle(position, offset, max_speed, particle_count, particle)
                .await;
        }
    }

    pub async fn play_sound(&self, sound: Sound, category: SoundCategory, position: &Vector3<f64>) {
        self.play_sound_raw(sound as u16, category, position, 1.0, 1.0)
            .await;
    }

    pub async fn play_sound_raw(
        &self,
        sound_id: u16,
        category: SoundCategory,
        position: &Vector3<f64>,
        volume: f32,
        pitch: f32,
    ) {
        let seed = thread_rng().r#gen::<f64>();
        let packet = CSoundEffect::new(IdOr::Id(sound_id), category, position, volume, pitch, seed);
        self.broadcast_packet_all(&packet).await;
    }

    pub async fn play_block_sound(
        &self,
        sound: Sound,
        category: SoundCategory,
        position: BlockPos,
    ) {
        let new_vec = Vector3::new(
            f64::from(position.0.x) + 0.5,
            f64::from(position.0.y) + 0.5,
            f64::from(position.0.z) + 0.5,
        );
        self.play_sound(sound, category, &new_vec).await;
    }

    pub async fn play_record(&self, record_id: i32, position: BlockPos) {
        self.broadcast_packet_all(&CLevelEvent::new(
            WorldEvent::JukeboxStartsPlaying as i32,
            position,
            record_id,
            false,
        ))
        .await;
    }

    pub async fn stop_record(&self, position: BlockPos) {
        self.broadcast_packet_all(&CLevelEvent::new(
            WorldEvent::JukeboxStopsPlaying as i32,
            position,
            0,
            false,
        ))
        .await;
    }

    pub async fn tick(self: &Arc<Self>, server: &Server) {
        self.flush_block_updates().await;

        // world ticks
        {
            let mut level_time = self.level_time.lock().await;
            level_time.tick_time();
            if level_time.world_age % 20 == 0 {
                level_time.send_time(self).await;
            }
        }

        {
            let mut weather = self.weather.lock().await;
            weather.tick_weather(self).await;
        };

        self.tick_scheduled_block_ticks().await;

        // player ticks
        for player in self.players.read().await.values() {
            player.tick(server).await;
        }

        let entities_to_tick: Vec<_> = self.entities.read().await.values().cloned().collect();

        // Entity ticks
        for entity in entities_to_tick {
            entity.tick(server).await;
            // This boolean thing prevents deadlocks. Since we lock players, we can't broadcast packets.
            let mut collied_player = None;
            for player in self.players.read().await.values() {
                if player
                    .living_entity
                    .entity
                    .bounding_box
                    .load()
                    // This is vanilla, but TODO: change this when is in a vehicle
                    .expand(1.0, 0.5, 1.0)
                    .intersects(&entity.get_entity().bounding_box.load())
                {
                    collied_player = Some(player.clone());
                    break;
                }
            }
            if let Some(player) = collied_player {
                entity.on_player_collision(player).await;
            }
        }
    }

    pub async fn flush_block_updates(&self) {
        let mut block_state_updates_by_chunk_section = HashMap::new();
        for (position, block_state_id) in self.unsent_block_changes.lock().await.drain() {
            let chunk_section = chunk_section_from_pos(&position);
            block_state_updates_by_chunk_section
                .entry(chunk_section)
                .or_insert(Vec::new())
                .push((position, block_state_id));
        }

        // TODO: only send packet to players who have the chunks loaded
        // TODO: Send light updates to update the wire directly next to a broken block
        for chunk_section in block_state_updates_by_chunk_section.values() {
            if chunk_section.is_empty() {
                continue;
            }
            if chunk_section.len() == 1 {
                let (block_pos, block_state_id) = chunk_section[0];
                self.broadcast_packet_all(&CBlockUpdate::new(
                    block_pos,
                    i32::from(block_state_id).into(),
                ))
                .await;
            } else {
                self.broadcast_packet_all(&CMultiBlockUpdate::new(chunk_section.clone()))
                    .await;
            }
        }
    }

    pub async fn tick_scheduled_block_ticks(self: &Arc<Self>) {
        let blocks_to_tick = self.level.get_and_tick_block_ticks().await;
        let fluids_to_tick = self.level.get_and_tick_fluid_ticks().await;

        for scheduled_tick in blocks_to_tick {
            let block = self.get_block(&scheduled_tick.block_pos).await.unwrap();
            if scheduled_tick.target_block_id != block.id {
                continue;
            }
            if let Some(pumpkin_block) = self.block_registry.get_pumpkin_block(&block) {
                pumpkin_block
                    .on_scheduled_tick(self, &block, &scheduled_tick.block_pos)
                    .await;
            }
        }

        for scheduled_tick in fluids_to_tick {
            let Ok(fluid) = self.get_fluid(&scheduled_tick.block_pos).await else {
                continue;
            };
            if scheduled_tick.target_block_id != fluid.id {
                continue;
            }
            if let Some(pumpkin_fluid) = self.block_registry.get_pumpkin_fluid(&fluid) {
                pumpkin_fluid
                    .on_scheduled_tick(self, &fluid, &scheduled_tick.block_pos)
                    .await;
            }
        }
    }

    /// Gets the y position of the first non air block from the top down
    pub async fn get_top_block(&self, position: Vector2<i32>) -> i32 {
        for y in (-64..=319).rev() {
            let pos = BlockPos(Vector3::new(position.x, y, position.z));
            let block = self.get_block_state(&pos).await;
            if let Ok(block) = block {
                if block.air {
                    continue;
                }
            }
            return y;
        }
        319
    }

    #[expect(clippy::too_many_lines)]
    pub async fn spawn_player(
        &self,
        base_config: &BasicConfiguration,
        player: Arc<Player>,
        server: &Server,
    ) {
        let dimensions: Vec<Identifier> =
            server.dimensions.iter().map(DimensionType::name).collect();

        // This code follows the vanilla packet order
        let entity_id = player.entity_id();
        let gamemode = player.gamemode.load();
        log::debug!(
            "spawning player {}, entity id {}",
            player.gameprofile.name,
            entity_id
        );

        // Send the login packet for our new player
        player
            .client
            .send_packet_now(&CLogin::new(
                entity_id,
                base_config.hardcore,
                &dimensions,
                base_config.max_players.try_into().unwrap(),
                base_config.view_distance.get().into(), //  TODO: view distance
                base_config.simulation_distance.get().into(), // TODO: sim view dinstance
                false,
                true,
                false,
                (self.dimension_type as u8).into(),
                self.dimension_type.name(),
                biome::hash_seed(self.level.seed.0), // seed
                gamemode as u8,
                player
                    .previous_gamemode
                    .load()
                    .map_or(-1, |gamemode| gamemode as i8),
                false,
                false,
                None,
                0.into(),
                self.sea_level.into(),
                // This should stay true even when reports are disabled.
                // It prevents the annoying popup when joining the server.
                true,
            ))
            .await;
        // Permissions, i.e. the commands a player may use.
        player.send_permission_lvl_update().await;
        {
            let command_dispatcher = server.command_dispatcher.read().await;
            client_suggestions::send_c_commands_packet(&player, &command_dispatcher).await;
        };

        // Teleport
        let (position, yaw, pitch) = if player.has_played_before.load(Ordering::Relaxed) {
            let position = player.position();
            let yaw = player.living_entity.entity.yaw.load(); //info.spawn_angle;
            let pitch = player.living_entity.entity.pitch.load();

            (position, yaw, pitch)
        } else {
            let info = &self.level.level_info;
            let position = Vector3::new(
                f64::from(info.spawn_x),
                f64::from(info.spawn_y) + 1.0,
                f64::from(info.spawn_z),
            );
            let yaw = info.spawn_angle;
            let pitch = 0.0;

            (position, yaw, pitch)
        };

        let velocity = player.living_entity.entity.velocity.load();

        log::debug!("Sending player teleport to {}", player.gameprofile.name);
        player.request_teleport(position, yaw, pitch).await;

        player.living_entity.last_pos.store(position);

        let gameprofile = &player.gameprofile;
        // Firstly, send an info update to our new player, so they can see their skin
        // and also send their info to everyone else.
        log::debug!("Broadcasting player info for {}", player.gameprofile.name);
        self.broadcast_packet_all(&CPlayerInfoUpdate::new(
            (PlayerInfoFlags::ADD_PLAYER | PlayerInfoFlags::UPDATE_GAME_MODE).bits(),
            &[pumpkin_protocol::client::play::Player {
                uuid: gameprofile.id,
                actions: &[
                    PlayerAction::AddPlayer {
                        name: &gameprofile.name,
                        properties: &gameprofile.properties,
                    },
                    PlayerAction::UpdateGameMode(VarInt(gamemode as i32)),
                ],
            }],
        ))
        .await;

        // Here, we send all the infos of players who already joined.
        {
            let current_players = self.players.read().await;

            let mut current_player_data = Vec::new();

            for (_, player) in current_players
                .iter()
                .filter(|(c, _)| **c != player.gameprofile.id)
            {
                let chat_session = player.chat_session.lock().await;

                let mut player_actions = vec![PlayerAction::AddPlayer {
                    name: &player.gameprofile.name,
                    properties: &player.gameprofile.properties,
                }];

                if base_config.allow_chat_reports {
                    player_actions.push(PlayerAction::InitializeChat(Some(InitChat {
                        session_id: chat_session.session_id,
                        expires_at: chat_session.expires_at,
                        public_key: chat_session.public_key.clone(),
                        signature: chat_session.signature.clone(),
                    })));
                }

                current_player_data.push((&player.gameprofile.id, player_actions));
            }

            let mut action_flags = PlayerInfoFlags::ADD_PLAYER;
            if base_config.allow_chat_reports {
                action_flags |= PlayerInfoFlags::INITIALIZE_CHAT;
            }

            let entries = current_player_data
                .iter()
                .map(|(id, actions)| pumpkin_protocol::client::play::Player {
                    uuid: **id,
                    actions,
                })
                .collect::<Vec<_>>();

            log::debug!("Sending player info to {}", player.gameprofile.name);
            player
                .client
                .enqueue_packet(&CPlayerInfoUpdate::new(action_flags.bits(), &entries))
                .await;
        };

        let gameprofile = &player.gameprofile;

        log::debug!("Broadcasting player spawn for {}", player.gameprofile.name);
        // Spawn the player for every client.
        self.broadcast_packet_except(
            &[player.gameprofile.id],
            &CSpawnEntity::new(
                entity_id.into(),
                gameprofile.id,
                i32::from(EntityType::PLAYER.id).into(),
                position,
                pitch,
                yaw,
                yaw,
                0.into(),
                velocity,
            ),
        )
        .await;

        // Spawn players for our client.
        let id = player.gameprofile.id;
        for (_, existing_player) in self.players.read().await.iter().filter(|c| c.0 != &id) {
            let entity = &existing_player.living_entity.entity;
            let pos = entity.pos.load();
            let gameprofile = &existing_player.gameprofile;
            log::debug!("Sending player entities to {}", player.gameprofile.name);

            player
                .client
                .enqueue_packet(&CSpawnEntity::new(
                    existing_player.entity_id().into(),
                    gameprofile.id,
                    i32::from(EntityType::PLAYER.id).into(),
                    pos,
                    entity.yaw.load(),
                    entity.pitch.load(),
                    entity.head_yaw.load(),
                    0.into(),
                    entity.velocity.load(),
                ))
                .await;
        }

        // Set skin parts and tablist
        for player in self.players.read().await.values() {
            //Set / Update skin part for every player
            player.send_client_information().await;

            //Update tablist
            //For the tablist to update, the player (who is not on the tablist) needs to send the packet and the tablist will be updated for every player
            self.broadcast_packet_all(&CPlayerInfoUpdate::new(
                0x08,
                &[pumpkin_protocol::client::play::Player {
                    uuid: player.gameprofile.id,
                    actions: &[PlayerAction::UpdateListed(true)],
                }],
            ))
            .await;
        }

        // Start waiting for level chunks. Sets the "Loading Terrain" screen
        log::debug!("Sending waiting chunks to {}", player.gameprofile.name);
        player
            .client
            .send_packet_now(&CGameEvent::new(GameEvent::StartWaitingChunks, 0.0))
            .await;

        self.worldborder
            .lock()
            .await
            .init_client(&player.client)
            .await;

        // Sends initial time
        player.send_time(self).await;

        // Send initial weather state
        let weather = self.weather.lock().await;
        if weather.raining {
            player
                .client
                .enqueue_packet(&CGameEvent::new(GameEvent::BeginRaining, 0.0))
                .await;

            // Calculate rain and thunder levels directly from public fields
            let rain_level = weather.rain_level.clamp(0.0, 1.0);
            let thunder_level = weather.thunder_level.clamp(0.0, 1.0);

            player
                .client
                .enqueue_packet(&CGameEvent::new(GameEvent::RainLevelChange, rain_level))
                .await;
            player
                .client
                .enqueue_packet(&CGameEvent::new(
                    GameEvent::ThunderLevelChange,
                    thunder_level,
                ))
                .await;
        }

        // Spawn in initial chunks
        chunker::player_join(&player).await;

        // if let Some(bossbars) = self..lock().await.get_player_bars(&player.gameprofile.id) {
        //     for bossbar in bossbars {
        //         player.send_bossbar(bossbar).await;
        //     }
        // }

        player.has_played_before.store(true, Ordering::Relaxed);
        player.send_mobs(self).await;
        player.send_inventory().await;
    }

    pub async fn send_world_info(
        &self,
        player: &Arc<Player>,
        position: Vector3<f64>,
        yaw: f32,
        pitch: f32,
    ) {
        self.worldborder
            .lock()
            .await
            .init_client(&player.client)
            .await;

        // TODO: World spawn (compass stuff)

        player
            .client
            .enqueue_packet(&CGameEvent::new(GameEvent::StartWaitingChunks, 0.0))
            .await;

        let entity = &player.living_entity.entity;

        self.broadcast_packet_except(
            &[player.gameprofile.id],
            // TODO: add velo
            &CSpawnEntity::new(
                entity.entity_id.into(),
                player.gameprofile.id,
                i32::from(EntityType::PLAYER.id).into(),
                position,
                pitch,
                yaw,
                yaw,
                0.into(),
                Vector3::new(0.0, 0.0, 0.0),
            ),
        )
        .await;
        player.send_client_information().await;

        chunker::player_join(player).await;
        // Update commands

        player.set_health(20.0).await;
    }

    pub async fn explode(self: &Arc<Self>, server: &Server, position: Vector3<f64>, power: f32) {
        let explosion = Explosion::new(power, position);
        explosion.explode(server, self).await;
        let particle = if power < 2.0 {
            Particle::Explosion
        } else {
            Particle::ExplosionEmitter
        };
        let sound = IdOr::<SoundEvent>::Id(Sound::EntityGenericExplode as u16);
        for (_, player) in self.players.read().await.iter() {
            if player.position().squared_distance_to_vec(position) > 4096.0 {
                continue;
            }
            player
                .client
                .enqueue_packet(&CExplosion::new(
                    position,
                    None,
                    VarInt(particle as i32),
                    sound.clone(),
                ))
                .await;
        }
    }

    pub async fn respawn_player(&self, player: &Arc<Player>, alive: bool) {
        let last_pos = player.living_entity.last_pos.load();
        let death_dimension = player.world().await.dimension_type.name();
        let death_location = BlockPos(Vector3::new(
            last_pos.x.round() as i32,
            last_pos.y.round() as i32,
            last_pos.z.round() as i32,
        ));

        let data_kept = u8::from(alive);

        // TODO: switch world in player entity to new world

        player
            .client
            .enqueue_packet(&CRespawn::new(
                (self.dimension_type as u8).into(),
                self.dimension_type.name(),
                biome::hash_seed(self.level.seed.0), // seed
                player.gamemode.load() as u8,
                player.gamemode.load() as i8,
                false,
                false,
                Some((death_dimension, death_location)),
                0.into(),
                self.sea_level.into(),
                data_kept,
            ))
            .await;

        log::debug!("Sending player abilities to {}", player.gameprofile.name);
        player.send_abilities_update().await;

        player.send_permission_lvl_update().await;

        // Teleport
        let info = &self.level.level_info;
        let mut position = Vector3::new(
            f64::from(info.spawn_x),
            f64::from(info.spawn_y),
            f64::from(info.spawn_z),
        );
        let yaw = info.spawn_angle;
        let pitch = 0.0;

        let top = self
            .get_top_block(Vector2::new(position.x as i32, position.z as i32))
            .await;
        position.y = f64::from(top + 1);

        log::debug!("Sending player teleport to {}", player.gameprofile.name);
        player.clone().request_teleport(position, yaw, pitch).await;

        player.living_entity.last_pos.store(position);

        // TODO: difficulty, exp bar, status effect

        self.send_world_info(player, position, yaw, pitch).await;
    }

    // NOTE: This function doesn't actually await on anything, it just spawns two tokio tasks
    /// IMPORTANT: Chunks have to be non-empty
    #[allow(clippy::too_many_lines)]
    fn spawn_world_chunks(
        &self,
        player: Arc<Player>,
        chunks: Vec<Vector2<i32>>,
        center_chunk: Vector2<i32>,
    ) {
        if player
            .client
            .closed
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            log::info!("The connection has closed before world chunks were spawned");
            return;
        }
        #[cfg(debug_assertions)]
        let inst = std::time::Instant::now();

        // Sort such that the first chunks are closest to the center.
        let mut chunks = chunks;
        chunks.sort_unstable_by_key(|pos| {
            let rel_x = pos.x - center_chunk.x;
            let rel_z = pos.z - center_chunk.z;
            rel_x * rel_x + rel_z * rel_z
        });

        let mut receiver = self.receive_chunks(chunks);
        let level = self.level.clone();

        player.clone().spawn_task(async move {
            'main: loop {
                let recv_result = tokio::select! {
                    () = player.client.await_close_interrupt() => {
                        log::debug!("Canceling player packet processing");
                        None
                    },
                    recv_result = receiver.recv() => {
                        recv_result
                    }
                };

                let Some((chunk, first_load)) = recv_result else {
                    break;
                };

                let position = chunk.read().await.position;

                #[cfg(debug_assertions)]
                if position == (0, 0).into() {
                    use pumpkin_protocol::client::play::CChunkData;
                    let binding = chunk.read().await;
                    let packet = CChunkData(&binding);
                    let mut test = Vec::new();
                    packet.write_packet_data(&mut test).unwrap();
                    let len = test.len();
                    log::debug!(
                        "Chunk packet size: {}B {}KB {}MB",
                        len,
                        len / 1024,
                        len / (1024 * 1024)
                    );
                }

                let (world, chunk) = if level.is_chunk_watched(&position) {
                    (player.world().await.clone(), chunk)
                } else {
                    send_cancellable! {{
                        ChunkSave {
                            world: player.world().await.clone(),
                            chunk,
                            cancelled: false,
                        };

                        'after: {
                            log::trace!(
                                "Received chunk {:?}, but it is no longer watched... cleaning",
                                &position
                            );
                            level.clean_chunk(&position).await;
                            continue 'main;
                        }
                    }};
                    (event.world, event.chunk)
                };

                let (world, chunk) = if first_load {
                    send_cancellable! {{
                        ChunkLoad {
                            world,
                            chunk,
                            cancelled: false,
                        };

                        'cancelled: {
                            continue 'main;
                        }
                    }}
                    (event.world, event.chunk)
                } else {
                    (world, chunk)
                };

                if !player.client.closed.load(Ordering::Relaxed) {
                    send_cancellable! {{
                        ChunkSend {
                            world,
                            chunk: chunk.clone(),
                            cancelled: false,
                        };

                        'after: {
                            let mut chunk_manager = player.chunk_manager.lock().await;
                            chunk_manager.push_chunk(position, chunk);
                        }
                    }};
                }
            }

            #[cfg(debug_assertions)]
            log::debug!("Chunks queued after {}ms", inst.elapsed().as_millis());
        });
    }

    /// Gets a `Player` by an entity id
    pub async fn get_player_by_id(&self, id: EntityId) -> Option<Arc<Player>> {
        for player in self.players.read().await.values() {
            if player.entity_id() == id {
                return Some(player.clone());
            }
        }
        None
    }

    /// Gets an entity by an entity id
    pub async fn get_entity_by_id(&self, id: EntityId) -> Option<Arc<dyn EntityBase>> {
        for entity in self.entities.read().await.values() {
            if entity.get_entity().entity_id == id {
                return Some(entity.clone());
            }
        }
        None
    }

    /// Gets a `Player` by a username
    pub async fn get_player_by_name(&self, name: &str) -> Option<Arc<Player>> {
        for player in self.players.read().await.values() {
            if player.gameprofile.name.to_lowercase() == name.to_lowercase() {
                return Some(player.clone());
            }
        }
        None
    }

    /// Retrieves a player by their unique UUID.
    ///
    /// This function searches the world's active player list for a player with the specified UUID.
    /// If found, it returns an `Arc<Player>` reference to the player. Otherwise, it returns `None`.
    ///
    /// # Arguments
    ///
    /// * `id`: The UUID of the player to retrieve.
    ///
    /// # Returns
    ///
    /// An `Option<Arc<Player>>` containing the player if found, or `None` if not.
    pub async fn get_player_by_uuid(&self, id: uuid::Uuid) -> Option<Arc<Player>> {
        return self.players.read().await.get(&id).cloned();
    }

    /// Gets a list of players whose location equals the given position in the world.
    ///
    /// It iterates through the players in the world and checks their location. If the player's location matches the
    /// given position, it will add this to a `Vec` which it later returns. If no
    /// player was found in that position, it will just return an empty `Vec`.
    ///
    /// # Arguments
    ///
    /// * `position`: The position the function will check.
    pub async fn get_players_by_pos(&self, position: BlockPos) -> HashMap<uuid::Uuid, Arc<Player>> {
        self.players
            .read()
            .await
            .iter()
            .filter_map(|(uuid, player)| {
                let player_block_pos = player.living_entity.entity.block_pos.load().0;
                (position.0.x == player_block_pos.x
                    && position.0.y == player_block_pos.y
                    && position.0.z == player_block_pos.z)
                    .then(|| (*uuid, Arc::clone(player)))
            })
            .collect::<HashMap<uuid::Uuid, Arc<Player>>>()
    }

    /// Gets the nearby players around a given world position.
    /// It "creates" a sphere and checks if whether players are inside
    /// and returns a `HashMap` where the UUID is the key and the `Player`
    /// object is the value.
    ///
    /// # Arguments
    /// * `pos`: The center of the sphere.
    /// * `radius`: The radius of the sphere. The higher the radius, the more area will be checked (in every direction).
    pub async fn get_nearby_players(
        &self,
        pos: Vector3<f64>,
        radius: f64,
    ) -> HashMap<uuid::Uuid, Arc<Player>> {
        let radius_squared = radius.powi(2);

        self.players
            .read()
            .await
            .iter()
            .filter_map(|(id, player)| {
                let player_pos = player.living_entity.entity.pos.load();
                (player_pos.squared_distance_to_vec(pos) <= radius_squared)
                    .then(|| (*id, player.clone()))
            })
            .collect()
    }

    pub async fn get_closest_player(&self, pos: Vector3<f64>, radius: f64) -> Option<Arc<Player>> {
        let players = self.get_nearby_players(pos, radius).await;
        players
            .iter()
            .min_by(|a, b| {
                a.1.living_entity
                    .entity
                    .pos
                    .load()
                    .squared_distance_to_vec(pos)
                    .partial_cmp(
                        &b.1.living_entity
                            .entity
                            .pos
                            .load()
                            .squared_distance_to_vec(pos),
                    )
                    .unwrap()
            })
            .map(|p| p.1.clone())
    }

    /// Adds a player to the world and broadcasts a join message if enabled.
    ///
    /// This function takes a player's UUID and an `Arc<Player>` reference.
    /// It inserts the player into the world's `current_players` map using the UUID as the key.
    /// Additionally, it broadcasts a join message to all connected players in the world.
    ///
    /// # Arguments
    ///
    /// * `uuid`: The unique UUID of the player to add.
    /// * `player`: An `Arc<Player>` reference to the player object.
    pub async fn add_player(&self, uuid: uuid::Uuid, player: Arc<Player>) {
        {
            let mut current_players = self.players.write().await;
            current_players.insert(uuid, player.clone())
        };

        let current_players = self.players.clone();
        player.clone().spawn_task(async move {
            let msg_comp = TextComponent::translate(
                "multiplayer.player.joined",
                [TextComponent::text(player.gameprofile.name.clone())],
            )
            .color_named(NamedColor::Yellow);
            let event = PlayerJoinEvent::new(player.clone(), msg_comp);

            let event = PLUGIN_MANAGER
                .lock()
                .await
                .fire::<PlayerJoinEvent>(event)
                .await;

            if !event.cancelled {
                let current_players = current_players.clone();
                let players = current_players.read().await;
                for player in players.values() {
                    player.send_system_message(&event.join_message).await;
                }
                log::info!("{}", event.join_message.clone().to_pretty_console());
            }
        });
    }

    /// Removes a player from the world and broadcasts a disconnect message if enabled.
    ///
    /// This function removes a player from the world based on their `Player` reference.
    /// It performs the following actions:
    ///
    /// 1. Removes the player from the `current_players` map using their UUID.
    /// 2. Broadcasts a `CRemovePlayerInfo` packet to all connected players to inform them about the player leaving.
    /// 3. Removes the player's entity from the world using its entity ID.
    /// 4. Optionally sends a disconnect message to all other players notifying them about the player leaving.
    ///
    /// # Arguments
    ///
    /// * `player`: A reference to the `Player` object to be removed.
    /// * `fire_event`: A boolean flag indicating whether to fire a `PlayerLeaveEvent` event.
    ///
    /// # Notes
    ///
    /// - This function assumes `broadcast_packet_expect` and `remove_entity` are defined elsewhere.
    /// - The disconnect message sending is currently optional. Consider making it a configurable option.
    pub async fn remove_player(&self, player: &Arc<Player>, fire_event: bool) {
        self.players
            .write()
            .await
            .remove(&player.gameprofile.id)
            .unwrap();
        let uuid = player.gameprofile.id;
        self.broadcast_packet_except(&[player.gameprofile.id], &CRemovePlayerInfo::new(&[uuid]))
            .await;
        self.broadcast_packet_all(&CRemoveEntities::new(&[player.entity_id().into()]))
            .await;

        if fire_event {
            let msg_comp = TextComponent::translate(
                "multiplayer.player.left",
                [TextComponent::text(player.gameprofile.name.clone())],
            )
            .color_named(NamedColor::Yellow);
            let event = PlayerLeaveEvent::new(player.clone(), msg_comp);

            let event = PLUGIN_MANAGER
                .lock()
                .await
                .fire::<PlayerLeaveEvent>(event)
                .await;

            if !event.cancelled {
                let players = self.players.read().await;
                for player in players.values() {
                    player.send_system_message(&event.leave_message).await;
                }
                log::info!("{}", event.leave_message.clone().to_pretty_console());
            }
        }
    }

    pub fn create_entity(
        self: &Arc<Self>,
        position: Vector3<f64>,
        entity_type: EntityType,
    ) -> Entity {
        let uuid = uuid::Uuid::new_v4();
        Entity::new(uuid, self.clone(), position, entity_type, false)
    }

    /// Adds an entity to the world.
    pub async fn spawn_entity(&self, entity: Arc<dyn EntityBase>) {
        let base_entity = entity.get_entity();

        if base_entity.entity_type == EntityType::NPC {
            self.broadcast_packet_all(&CPlayerInfoUpdate::new(
                PlayerInfoFlags::ADD_PLAYER.bits(),
                &[pumpkin_protocol::client::play::Player {
                    uuid: entity.get_entity().entity_uuid,
                    actions: &[AddPlayer {
                        name: "NPC",
                        properties: &[],
                    }],
                }],
            ))
            .await;
        }
        self.broadcast_packet_all(&base_entity.create_spawn_packet())
            .await;
        let mut current_living_entities = self.entities.write().await;
        current_living_entities.insert(base_entity.entity_uuid, entity);
    }

    pub async fn remove_entity(&self, entity: &Entity) {
        self.entities.write().await.remove(&entity.entity_uuid);
        self.broadcast_packet_all(&CRemoveEntities::new(&[entity.entity_id.into()]))
            .await;
    }

    pub async fn set_block_breaking(&self, from: &Entity, location: BlockPos, progress: i32) {
        self.broadcast_packet_except(
            &[from.entity_uuid],
            &CSetBlockDestroyStage::new(from.entity_id.into(), location, progress as i8),
        )
        .await;
    }

    /// Sets a block and returns the old block id
    #[expect(clippy::too_many_lines)]
    pub async fn set_block_state(
        self: &Arc<Self>,
        position: &BlockPos,
        block_state_id: BlockStateId,
        flags: BlockFlags,
    ) -> BlockStateId {
        let chunk = self.get_chunk(position).await;
        let (_, relative) = position.chunk_and_chunk_relative_position();
        let mut chunk = chunk.write().await;
        let replaced_block_state_id = chunk
            .section
            .get_block_absolute_y(relative.x as usize, relative.y, relative.z as usize)
            .unwrap();

        if replaced_block_state_id == block_state_id {
            return block_state_id;
        }

        chunk.dirty = true;

        chunk.section.set_block_absolute_y(
            relative.x as usize,
            relative.y,
            relative.z as usize,
            block_state_id,
        );
        self.unsent_block_changes
            .lock()
            .await
            .insert(*position, block_state_id);
        drop(chunk);

        let old_block = Block::from_state_id(replaced_block_state_id).unwrap();
        let new_block = Block::from_state_id(block_state_id).unwrap();

        let block_moved = flags.contains(BlockFlags::MOVED);

        // WorldChunk.java line 310
        if old_block != new_block && (flags.contains(BlockFlags::NOTIFY_NEIGHBORS) || block_moved) {
            self.block_registry
                .on_state_replaced(
                    self,
                    &old_block,
                    *position,
                    replaced_block_state_id,
                    block_moved,
                )
                .await;
        }

        let block_state = self.get_block_state(position).await.unwrap();
        let new_block = Block::from_state_id(block_state_id).unwrap();
        let new_fluid = self.get_fluid(position).await.unwrap_or(Fluid::EMPTY);

        // WorldChunk.java line 318
        if !flags.contains(BlockFlags::SKIP_BLOCK_ADDED_CALLBACK) && new_block != old_block {
            self.block_registry
                .on_placed(
                    self,
                    &new_block,
                    block_state_id,
                    position,
                    replaced_block_state_id,
                    block_moved,
                )
                .await;
            self.block_registry
                .on_placed_fluid(
                    self,
                    &new_fluid,
                    block_state_id,
                    position,
                    replaced_block_state_id,
                    block_moved,
                )
                .await;
        }

        // Ig they do this cause it could be modified in chunkPos.setBlockState?
        if block_state.id == block_state_id {
            if flags.contains(BlockFlags::NOTIFY_LISTENERS) {
                // Mob AI update
            }

            if flags.contains(BlockFlags::NOTIFY_NEIGHBORS) {
                self.update_neighbors(position, None).await;
                // TODO: updateComparators
            }

            if !flags.contains(BlockFlags::FORCE_STATE) {
                let mut new_flags = flags;
                new_flags.remove(BlockFlags::NOTIFY_NEIGHBORS);
                new_flags.remove(BlockFlags::NOTIFY_LISTENERS);
                self.block_registry
                    .prepare(
                        self,
                        position,
                        &Block::from_state_id(replaced_block_state_id).unwrap(),
                        replaced_block_state_id,
                        new_flags,
                    )
                    .await;
                self.block_registry
                    .update_neighbors(
                        self,
                        position,
                        &Block::from_state_id(block_state_id).unwrap(),
                        new_flags,
                    )
                    .await;
                self.block_registry
                    .prepare(
                        self,
                        position,
                        &Block::from_state_id(block_state_id).unwrap(),
                        block_state_id,
                        new_flags,
                    )
                    .await;
            }
        }

        replaced_block_state_id
    }

    pub async fn schedule_block_tick(
        &self,
        block: &Block,
        block_pos: BlockPos,
        delay: u16,
        priority: TickPriority,
    ) {
        self.level
            .schedule_block_tick(block.id, block_pos, delay, priority)
            .await;
    }

    pub async fn schedule_fluid_tick(&self, block_id: u16, block_pos: BlockPos, delay: u16) {
        self.level
            .schedule_fluid_tick(block_id, &block_pos, delay)
            .await;
    }

    pub async fn is_block_tick_scheduled(&self, block_pos: &BlockPos, block: &Block) -> bool {
        self.level
            .is_block_tick_scheduled(block_pos, block.id)
            .await
    }
    // Stream the chunks (don't collect them and then do stuff with them)
    /// Spawns a tokio task to stream chunks.
    /// Important: must be called from an async function (or changed to accept a tokio runtime
    /// handle)
    pub fn receive_chunks(
        &self,
        chunks: Vec<Vector2<i32>>,
    ) -> UnboundedReceiver<(SyncChunk, bool)> {
        let (sender, receiver) = mpsc::unbounded_channel();
        // Put this in another thread so we aren't blocking on it
        let level = self.level.clone();
        self.level.spawn_task(async move {
            let cancel_notifier = level.shutdown_notifier.notified();
            let fetch_task = level.fetch_chunks(&chunks, sender);

            // Don't continue to handle chunks if we are shutting down
            select! {
                () = cancel_notifier => {},
                () = fetch_task => {}
            };
        });

        receiver
    }

    pub async fn receive_chunk(&self, chunk_pos: Vector2<i32>) -> (Arc<RwLock<ChunkData>>, bool) {
        let mut receiver = self.receive_chunks(vec![chunk_pos]);

        receiver
            .recv()
            .await
            .expect("Channel closed for unknown reason")
    }

    pub async fn break_block(
        self: &Arc<Self>,
        position: &BlockPos,
        cause: Option<Arc<Player>>,
        flags: BlockFlags,
    ) {
        let block = self.get_block(position).await.unwrap();
        let event = BlockBreakEvent::new(cause.clone(), block.clone(), *position, 0, false);

        let event = PLUGIN_MANAGER
            .lock()
            .await
            .fire::<BlockBreakEvent>(event)
            .await;

        if !event.cancelled {
            let broken_block_state_id = self.set_block_state(position, 0, flags).await;

            let particles_packet = CWorldEvent::new(
                WorldEvent::BlockBroken as i32,
                *position,
                broken_block_state_id.into(),
                false,
            );

            if !flags.contains(BlockFlags::SKIP_DROPS) {
                block::drop_loot(self, &block, position, true, broken_block_state_id).await;
            }

            match cause {
                Some(player) => {
                    self.broadcast_packet_except(&[player.gameprofile.id], &particles_packet)
                        .await;
                }
                None => self.broadcast_packet_all(&particles_packet).await,
            }
        }
    }

    pub async fn get_chunk(&self, position: &BlockPos) -> Arc<RwLock<ChunkData>> {
        let (chunk_coordinate, _) = position.chunk_and_chunk_relative_position();

        match self.level.try_get_chunk(chunk_coordinate) {
            Some(chunk) => chunk.clone(),
            None => self.receive_chunk(chunk_coordinate).await.0,
        }
    }

    pub async fn get_block_state_id(
        &self,
        position: &BlockPos,
    ) -> Result<BlockStateId, GetBlockError> {
        let chunk = self.get_chunk(position).await;
        let (_, relative) = position.chunk_and_chunk_relative_position();

        let chunk = chunk.read().await;
        let Some(id) = chunk.section.get_block_absolute_y(
            relative.x as usize,
            relative.y,
            relative.z as usize,
        ) else {
            return Err(GetBlockError::BlockOutOfWorldBounds);
        };

        Ok(id)
    }

    /// Gets a `Block` from the block registry. Returns `None` if the block was not found.
    pub async fn get_block(
        &self,
        position: &BlockPos,
    ) -> Result<pumpkin_data::block::Block, GetBlockError> {
        let id = self.get_block_state_id(position).await?;
        get_block_by_state_id(id).ok_or(GetBlockError::InvalidBlockId)
    }

    pub async fn get_fluid(
        &self,
        position: &BlockPos,
    ) -> Result<pumpkin_data::fluid::Fluid, GetBlockError> {
        let id = self.get_block_state_id(position).await?;
        Fluid::from_state_id(id).ok_or(GetBlockError::InvalidBlockId)
    }

    /// Gets the `BlockState` from the block registry. Returns `None` if the block state was not found.
    pub async fn get_block_state(
        &self,
        position: &BlockPos,
    ) -> Result<pumpkin_data::block::BlockState, GetBlockError> {
        let id = self.get_block_state_id(position).await?;
        get_state_by_state_id(id).ok_or(GetBlockError::InvalidBlockId)
    }

    pub fn get_state_by_id(
        &self,
        id: u16,
    ) -> Result<pumpkin_data::block::BlockState, GetBlockError> {
        get_state_by_state_id(id).ok_or(GetBlockError::InvalidBlockId)
    }

    /// Gets the Block + Block state from the Block Registry, Returns None if the Block state has not been found
    pub async fn get_block_and_block_state(
        &self,
        position: &BlockPos,
    ) -> Result<(pumpkin_data::block::Block, pumpkin_data::block::BlockState), GetBlockError> {
        let id = self.get_block_state_id(position).await?;
        get_block_and_state_by_state_id(id).ok_or(GetBlockError::InvalidBlockId)
    }

    /// Updates neighboring blocks of a block
    pub async fn update_neighbors(
        self: &Arc<Self>,
        block_pos: &BlockPos,
        except: Option<&BlockDirection>,
    ) {
        let source_block = self.get_block(block_pos).await.unwrap();
        for direction in BlockDirection::update_order() {
            if Some(&direction) == except {
                continue;
            }
            let neighbor_pos = block_pos.offset(direction.to_offset());
            let neighbor_block = self.get_block(&neighbor_pos).await;
            let neighbor_fluid = self.get_fluid(&neighbor_pos).await;
            if let Ok(neighbor_block) = neighbor_block {
                if let Some(neighbor_pumpkin_block) =
                    self.block_registry.get_pumpkin_block(&neighbor_block)
                {
                    neighbor_pumpkin_block
                        .on_neighbor_update(
                            self,
                            &neighbor_block,
                            &neighbor_pos,
                            &source_block,
                            false,
                        )
                        .await;
                }
            }
            if let Ok(neighbor_fluid) = neighbor_fluid {
                if let Some(neighbor_pumpkin_fluid) =
                    self.block_registry.get_pumpkin_fluid(&neighbor_fluid)
                {
                    neighbor_pumpkin_fluid
                        .on_neighbor_update(self, &neighbor_fluid, &neighbor_pos, false)
                        .await;
                }
            }
        }
    }

    pub async fn update_neighbor(
        self: &Arc<Self>,
        neighbor_block_pos: &BlockPos,
        source_block: &Block,
    ) {
        let neighbor_block = self.get_block(neighbor_block_pos).await.unwrap();

        if let Some(neighbor_pumpkin_block) = self.block_registry.get_pumpkin_block(&neighbor_block)
        {
            neighbor_pumpkin_block
                .on_neighbor_update(
                    self,
                    &neighbor_block,
                    neighbor_block_pos,
                    source_block,
                    false,
                )
                .await;
        }
    }

    pub async fn replace_with_state_for_neighbor_update(
        self: &Arc<Self>,
        block_pos: &BlockPos,
        direction: &BlockDirection,
        flags: BlockFlags,
    ) {
        let (block, block_state) = match self.get_block_and_block_state(block_pos).await {
            Ok(block) => block,
            Err(_error) => {
                // Neighbor is outside the world. Don't try to update it
                return;
            }
        };

        if flags.contains(BlockFlags::SKIP_REDSTONE_WIRE_STATE_REPLACEMENT)
            && block.id == Block::REDSTONE_WIRE.id
        {
            return;
        }

        let neighbor_pos = block_pos.offset(direction.to_offset());
        let neighbor_state_id = self.get_block_state_id(&neighbor_pos).await.unwrap();

        let new_state_id = self
            .block_registry
            .get_state_for_neighbor_update(
                self,
                &block,
                block_state.id,
                block_pos,
                direction,
                &neighbor_pos,
                neighbor_state_id,
            )
            .await;

        if new_state_id != block_state.id {
            self.set_block_state(block_pos, new_state_id, flags).await;
        }
    }

    pub async fn get_block_entity(&self, block_pos: &BlockPos) -> Option<Arc<dyn BlockEntity>> {
        let chunk = self.get_chunk(block_pos).await;
        let chunk: tokio::sync::RwLockReadGuard<ChunkData> = chunk.read().await;

        chunk.block_entities.get(block_pos).cloned()
    }

    pub async fn add_block_entity(&self, block_entity: Arc<dyn BlockEntity>) {
        let block_pos = block_entity.get_position();
        let chunk = self.get_chunk(&block_pos).await;
        let mut chunk: tokio::sync::RwLockWriteGuard<ChunkData> = chunk.write().await;
        let block_entity_nbt = block_entity.chunk_data_nbt();

        if let Some(nbt) = block_entity_nbt {
            let mut bytes = Vec::new();
            to_bytes_unnamed(&nbt, &mut bytes).unwrap();
            self.broadcast_packet_all(&CBlockEntityData::new(
                block_entity.get_position(),
                VarInt(block_entity.get_id() as i32),
                bytes.into_boxed_slice(),
            ))
            .await;
        }

        chunk.block_entities.insert(block_pos, block_entity);
        chunk.dirty = true;
    }

    pub async fn remove_block_entity(&self, block_pos: &BlockPos) {
        let chunk = self.get_chunk(block_pos).await;
        let mut chunk: tokio::sync::RwLockWriteGuard<ChunkData> = chunk.write().await;
        chunk.block_entities.remove(block_pos);
        chunk.dirty = true;
    }
}
