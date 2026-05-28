//! Rebuild derived state from the canonical blocks already in the database.
//!
//! Re-executes every block forward from genesis over the stored headers and
//! bodies, rewriting the execution state, changesets, hashed state, trie, and
//! history indices. Headers, bodies, and receipts (the canonical block data)
//! are left untouched. The merkle stage validates each block's recomputed
//! state root against the canonical header, so a state divergence halts the
//! run at the offending block rather than persisting incorrect state.

use clap::Parser;
use reth_chainspec::{EthChainSpec, EthereumHardforks, Hardforks};
use reth_cli::chainspec::ChainSpecParser;
use reth_cli_commands::common::{
    AccessRights, CliComponentsBuilder, CliNodeComponents, CliNodeTypes, Environment,
    EnvironmentArgs,
};
use reth_config::Config;
use reth_consensus::noop::NoopConsensus;
use reth_evm::ConfigureEvm;
use reth_provider::{
    providers::ProviderNodeTypes, BlockNumReader, ChainSpecProvider, ProviderFactory,
};
use reth_stages::{sets::OfflineStages, Pipeline};
use reth_static_file::StaticFileProducer;
use tracing::*;

/// `arb-reth repair` command.
///
/// Reset derived state to genesis and re-execute all stored blocks forward,
/// validating each block's state root against its canonical header.
#[derive(Debug, Parser)]
pub struct Command<C: ChainSpecParser> {
    #[command(flatten)]
    env: EnvironmentArgs<C>,
}

impl<C: ChainSpecParser<ChainSpec: EthChainSpec + Hardforks + EthereumHardforks>> Command<C> {
    /// Execute the `repair` command.
    pub async fn execute<N>(
        self,
        components: impl CliComponentsBuilder<N>,
        runtime: reth_tasks::Runtime,
    ) -> eyre::Result<()>
    where
        N: CliNodeTypes<ChainSpec = C::ChainSpec>,
    {
        let Environment {
            provider_factory,
            config,
            ..
        } = self.env.init::<N>(AccessRights::RW, runtime)?;

        let components = components(provider_factory.chain_spec());
        let tip = provider_factory.provider()?.last_block_number()?;

        info!(
            target: "arb::repair",
            tip,
            "Rebuilding derived state from canonical blocks; headers and bodies are preserved"
        );

        let mut pipeline = build_pipeline(
            &config,
            provider_factory,
            components.evm_config().clone(),
            tip,
        );

        pipeline.move_to_static_files()?;

        // Reset the derived (offline) stages to genesis. Headers and bodies are
        // not part of the offline set, so the canonical block data is preserved.
        info!(target: "arb::repair", "Resetting derived state to genesis");
        pipeline.unwind(0, None)?;

        // Re-execute forward to the tip. The merkle stage validates each block's
        // recomputed state root against the canonical header; on mismatch the
        // pipeline fails on the unwind rather than persisting incorrect state.
        info!(target: "arb::repair", tip, "Re-executing forward");
        pipeline.run().await?;

        info!(
            target: "arb::repair",
            tip,
            "Repair complete; recomputed state root matches the canonical header at the tip"
        );
        Ok(())
    }
}

fn build_pipeline<N>(
    config: &Config,
    provider_factory: ProviderFactory<N>,
    evm_config: impl ConfigureEvm<Primitives = N::Primitives> + 'static,
    tip: u64,
) -> Pipeline<N>
where
    N: ProviderNodeTypes,
{
    let prune_modes = config.prune.segments.clone();
    Pipeline::<N>::builder()
        .with_max_block(tip)
        .with_fail_on_unwind(true)
        .add_stages(OfflineStages::new(
            evm_config,
            NoopConsensus::arc(),
            config.stages.clone(),
            prune_modes.clone(),
        ))
        .build(
            provider_factory.clone(),
            StaticFileProducer::new(provider_factory, prune_modes),
        )
}
