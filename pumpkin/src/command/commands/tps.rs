use async_trait::async_trait;
use pumpkin_config::BASIC_CONFIG;
use pumpkin_util::text::TextComponent;

use crate::command::{
    CommandError, CommandExecutor, CommandSender, args::ConsumedArgs, tree::CommandTree,
};

const NAMES: [&str; 1] = ["tps"];

const DESCRIPTION: &str = "Get server tps.";

struct Executor;

fn min(a: f64, b: f64) -> f64 {
    if a < b {
        a
    } else {
        b
    }
}

#[async_trait]
impl CommandExecutor for Executor {
    async fn execute<'a>(
        &self,
        sender: &mut CommandSender,
        server: &crate::server::Server,
        _args: &ConsumedArgs<'a>,
    ) -> Result<(), CommandError> {
        let nanos = server.nanos.read().await;
        let recent_1s = if nanos.len() <= 20 {
            &nanos[..]
        } else {
            &nanos[nanos.len() - 20..]
        };
        let recent_10s = if nanos.len() <= 200 {
            &nanos[..]
        } else {
            &nanos[nanos.len() - 200..]
        };
        let recent_1m = if nanos.len() <= 1200 {
            &nanos[..]
        } else {
            &nanos[nanos.len() - 1200..]
        };
        let mspt_1s = recent_1s.iter().sum::<u64>() as f64 / recent_1s.len() as f64 / 1_000_000.0;
        let mspt_10s =
            recent_10s.iter().sum::<u64>() as f64 / recent_10s.len() as f64 / 1_000_000.0;
        let mspt_1m = recent_1m.iter().sum::<u64>() as f64 / recent_1m.len() as f64 / 1_000_000.0;
        let tps_1s = min(BASIC_CONFIG.tps as f64, 1_000.0 / mspt_1s);
        let tps_10s = min(BASIC_CONFIG.tps as f64, 1_000.0 / mspt_10s);
        let tps_1m = min(BASIC_CONFIG.tps as f64, 1_000.0 / mspt_1m);
        sender
            .send_message(TextComponent::text(format!(
                "TPS: 1s: {:.2}, 10s: {:.2}, 1m: {:.2}",
                tps_1s, tps_10s, tps_1m
            )))
            .await;
        sender
            .send_message(TextComponent::text(format!(
                "MSPT: 1s: {:.2}, 10s: {:.2}, 1m: {:.2}",
                mspt_1s, mspt_10s, mspt_1m
            )))
            .await;
        Ok(())
    }
}

pub fn init_command_tree() -> CommandTree {
    CommandTree::new(NAMES, DESCRIPTION).execute(Executor)
}
