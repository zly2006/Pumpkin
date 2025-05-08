use async_trait::async_trait;
use pumpkin_util::text::TextComponent;

use crate::{
    command::{
        CommandError, CommandExecutor, CommandSender,
        args::{
            ConsumedArgs, FindArg, position_3d::Position3DArgumentConsumer,
            summonable_entities::SummonableEntitiesArgumentConsumer,
        },
        tree::{CommandTree, builder::argument},
    },
    entity::r#type::entity_base_from_type,
};
const NAMES: [&str; 1] = ["summon"];

const DESCRIPTION: &str = "Spawns a Entity at position.";

const ARG_ENTITY: &str = "entity";

const ARG_POS: &str = "pos";

struct Executor;

#[async_trait]
impl CommandExecutor for Executor {
    async fn execute<'a>(
        &self,
        sender: &mut CommandSender,
        _server: &crate::server::Server,
        args: &ConsumedArgs<'a>,
    ) -> Result<(), CommandError> {
        let entity_type = SummonableEntitiesArgumentConsumer::find_arg(args, ARG_ENTITY)?;
        let pos = Position3DArgumentConsumer::find_arg(args, ARG_POS);

        // TODO: Make this work in console
        if let Some(player) = sender.as_player() {
            let pos = pos.unwrap_or(player.living_entity.entity.pos.load());
            let entity = entity_base_from_type(
                entity_type,
                uuid::Uuid::new_v4(),
                player.world().await,
                pos,
                false,
            )
            .await;
            player.world().await.spawn_entity(entity).await;
            sender
                .send_message(TextComponent::translate(
                    "commands.summon.success",
                    [TextComponent::text(format!("{entity_type:?}"))],
                ))
                .await;
        }

        Ok(())
    }
}

pub fn init_command_tree() -> CommandTree {
    CommandTree::new(NAMES, DESCRIPTION).then(
        argument(ARG_ENTITY, SummonableEntitiesArgumentConsumer)
            .execute(Executor)
            .then(argument(ARG_POS, Position3DArgumentConsumer).execute(Executor)),
        // TODO: Add NBT
    )
}
