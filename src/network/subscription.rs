use std::collections::hash_map;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use nekoton_abi::FunctionExt;
use nekoton_utils::SimpleClock;
use rustc_hash::FxHashMap;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio_util::sync::{CancellationToken, DropGuard};
use ton_block::{Deserializable, Serializable};

use super::node_tcp_rpc::{ConfigWithId, NodeTcpRpc};
use super::node_udp_rpc::NodeUdpRpc;
use crate::util::{split_address, BlockStuff, FxDashMap, TransactionWithHash};

pub struct Subscription {
    node_tcp_rpc: NodeTcpRpc,
    node_udp_rpc: NodeUdpRpc,
    last_mc_block: ArcSwapOption<StoredMcBlock>,
    subscription_count: AtomicUsize,
    subscriptions_changed: Arc<Notify>,
    subscription_loop_step: Arc<Notify>,
    mc_subscriptions: AccountSubscriptions,
    sc_subscriptions: AccountSubscriptions,
    global_id: tokio::sync::Mutex<Option<i32>>,
    _cancellation: DropGuard,
}

impl Subscription {
    pub fn new(node_tcp_rpc: NodeTcpRpc, node_udp_rpc: NodeUdpRpc) -> Arc<Self> {
        let cancellation = CancellationToken::new();

        let subscription = Arc::new(Self {
            node_tcp_rpc,
            node_udp_rpc,
            last_mc_block: Default::default(),
            subscription_count: Default::default(),
            subscriptions_changed: Default::default(),
            subscription_loop_step: Default::default(),
            mc_subscriptions: Default::default(),
            sc_subscriptions: Default::default(),
            global_id: Default::default(),
            _cancellation: cancellation.clone().drop_guard(),
        });

        let walk_fut = walk_blocks(Arc::downgrade(&subscription));

        tokio::spawn(async move {
            tokio::select! {
                _ = walk_fut => {},
                _ = cancellation.cancelled() => {}
            }
        });

        subscription
    }

    pub async fn ensure_ready(&self) -> Result<()> {
        let (stats, capabilities) = futures_util::future::join(
            self.node_tcp_rpc.get_stats(),
            self.node_udp_rpc.get_capabilities(),
        )
        .await;

        stats
            .context("failed to get node stats")?
            .try_into_running()?;
        capabilities.context("failed to get node capabilities")?;

        Ok(())
    }

    pub fn tcp_rpc(&self) -> &NodeTcpRpc {
        &self.node_tcp_rpc
    }

    pub fn udp_rpc(&self) -> &NodeUdpRpc {
        &self.node_udp_rpc
    }

    pub async fn get_account_state(
        &self,
        address: &ton_block::MsgAddressInt,
    ) -> Result<Option<ton_block::AccountStuff>> {
        let state = self
            .node_tcp_rpc
            .get_shard_account_state(address)
            .await
            .context("failed to get shard account state")?;
        match state
            .read_account()
            .context("failed to read account state")?
        {
            ton_block::Account::Account(state) => Ok(Some(state)),
            ton_block::Account::AccountNone => Ok(None),
        }
    }

    pub async fn run_local(
        &self,
        address: &ton_block::MsgAddressInt,
        function: &ton_abi::Function,
        inputs: &[ton_abi::Token],
    ) -> Result<Vec<ton_abi::Token>> {
        let account = self
            .get_account_state(address)
            .await?
            .context("account not deployed")?;
        let output = function.run_local(&SimpleClock, account, inputs)?;
        match output.tokens {
            Some(tokens) => Ok(tokens),
            None => anyhow::bail!("getter failed (exit code: {})", output.result_code),
        }
    }

    pub async fn send_message_with_retires<F>(&self, mut f: F) -> Result<TransactionWithHash>
    where
        F: FnMut(u32, Option<i32>) -> Result<(ton_block::Message, u32)>,
    {
        let signature_id = self.get_signature_id().await?;

        let timeout = 60;
        loop {
            let (message, expire_at) = f(timeout, signature_id)?;
            if let Some(tx) = self.send_message(&message, expire_at).await? {
                break Ok(tx);
            }
        }
    }

    pub async fn send_message(
        &self,
        message: &ton_block::Message,
        expire_at: u32,
    ) -> Result<Option<TransactionWithHash>> {
        // Prepare dst address
        let raw_dst = match message.ext_in_header() {
            Some(header) => header.dst.clone(),
            None => anyhow::bail!("expected external message"),
        };
        let (workchain, dst) = split_address(&raw_dst)?;

        // Get message hash
        let msg_cell = message.serialize()?;
        let msg_hash = msg_cell.repr_hash();
        let data = ton_types::serialize_toc(&msg_cell)?;

        // Find pending messages map
        let subscriptions = match workchain {
            ton_block::MASTERCHAIN_ID => &self.mc_subscriptions,
            ton_block::BASE_WORKCHAIN_ID => &self.sc_subscriptions,
            _ => anyhow::bail!("unsupported workchain"),
        };

        // Insert pending message
        let (subscription_loop_works, rx) = {
            let mut subscription = subscriptions.entry(dst).or_default();

            let rx = match subscription.pending_messages.entry(msg_hash) {
                hash_map::Entry::Vacant(entry) => {
                    let (tx, rx) = oneshot::channel();
                    entry.insert(PendingMessage {
                        expire_at,
                        tx: Some(tx),
                    });
                    rx
                }
                hash_map::Entry::Occupied(_) => anyhow::bail!("message already sent"),
            };

            // Start waiting for the subscription loop to start
            let subscription_loop_works = self.subscription_loop_step.notified();

            // Notify waiters while pending messages is still acquired
            self.subscription_count.fetch_add(1, Ordering::Release);
            self.subscriptions_changed.notify_waiters();

            // Drop the lock
            (subscription_loop_works, rx)
        };

        // Wait until subscription loop was definitely started
        subscription_loop_works.await;

        // Send the message
        if let Err(e) = self.node_tcp_rpc.send_message(data).await {
            // Remove pending message from the map before returning an error
            match subscriptions.entry(dst) {
                dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                    let should_remove = {
                        let subscription = entry.get_mut();
                        subscription.pending_messages.remove(&msg_hash);
                        self.subscription_count.fetch_sub(1, Ordering::Release);
                        self.subscriptions_changed.notify_waiters();
                        subscription.is_empty()
                    };

                    if should_remove {
                        entry.remove();
                    }
                }
                dashmap::mapref::entry::Entry::Vacant(_) => {
                    tracing::warn!("pending messages entry not found");
                }
            };
            return Err(e);
        }
        tracing::debug!(dst = %raw_dst, ?msg_hash, "external message broadcasted");

        // Wait for the message execution
        let tx = rx.await?;
        match &tx {
            Some(tx) => {
                tracing::debug!(
                    dst = %raw_dst,
                    ?msg_hash,
                    tx_hash = ?tx.hash,
                    "external message delivered"
                );
            }
            None => {
                tracing::warn!(
                    dst = %raw_dst,
                    ?msg_hash,
                    "external message expired"
                );
            }
        }

        Ok(tx)
    }

    pub fn subscribe(&self, address: &ton_block::MsgAddressInt) -> TransactionsRx {
        let (tx, rx) = mpsc::unbounded_channel();
        let subscriptions = if address.workchain_id() == ton_block::MASTERCHAIN_ID {
            &self.mc_subscriptions
        } else {
            &self.sc_subscriptions
        };

        let address =
            ton_types::UInt256::from_le_bytes(&address.address().get_bytestring_on_stack(0));

        subscriptions
            .entry(address)
            .or_default()
            .transactions
            .push(tx);

        self.subscription_count.fetch_add(1, Ordering::Release);
        self.subscriptions_changed.notify_waiters();
        rx
    }

    pub async fn get_signature_id(&self) -> Result<Option<i32>> {
        let ConfigWithId { block_id, config } = self
            .node_tcp_rpc
            .get_config_all()
            .await
            .context("failed to get blockchain config")?;
        if !requires_signature_id(config.capabilities()) {
            return Ok(None);
        }

        let global_id = {
            let mut global_id = self.global_id.lock().await;
            match *global_id {
                // Once received, it will never change
                Some(global_id) => global_id,
                // Try to get the known masterchain block
                None => {
                    // TODO: replace with `global_id` from `getstats` when it will be available.
                    const RETRIES: usize = 10;
                    const INTERVAL: Duration = Duration::from_secs(1);

                    let mut retries = 0;
                    let block = loop {
                        match self.node_udp_rpc.get_block(&block_id).await {
                            Ok(block) => break block,
                            Err(e) if retries < RETRIES => {
                                tracing::error!("failed to get the latest mc block: {e:?}");
                                tokio::time::sleep(INTERVAL).await;
                                retries += 1;
                            }
                            Err(e) => return Err(e),
                        }
                    };

                    *global_id.insert(block.block().global_id)
                }
            }
        };

        Ok(Some(global_id))
    }

    async fn make_blocks_step(&self) -> Result<()> {
        // Get last masterchain block
        let last_mc_block = self
            .get_last_mc_block()
            .await
            .context("failed to get last mc block")?;

        self.subscription_loop_step.notify_waiters(); // messages barrier

        // Get next masterchain block
        let next_mc_block = self
            .node_udp_rpc
            .get_next_block(last_mc_block.data.id())
            .await
            .context("failed to get next block")?;
        let next_shard_block_ids = next_mc_block.shard_blocks()?;
        let next_mc_utime = {
            let info = next_mc_block.block().read_info()?;
            info.gen_utime().0
        };

        self.subscription_loop_step.notify_waiters(); // messages barrier

        tracing::debug!("next shard blocks: {next_shard_block_ids:#?}");

        // Get all shard blocks between these masterchain blocks
        let mut tasks = Vec::with_capacity(next_shard_block_ids.len());
        for id in next_shard_block_ids.values().cloned() {
            let last_mc_block = last_mc_block.clone();
            let rpc = self.node_udp_rpc.clone();
            tasks.push(tokio::spawn(async move {
                let edge = &last_mc_block.shards_edge;
                let mut blocks = Vec::new();

                let mut stack = Vec::from([id]);
                while let Some(id) = stack.pop() {
                    let block = rpc.get_block(&id).await?;
                    let info = block.read_brief_info()?;
                    blocks.push((info.gen_utime, block));

                    if edge.is_before(&info.prev1) {
                        stack.push(info.prev1);
                    }
                    if let Some(prev_id2) = info.prev2 {
                        if edge.is_before(&prev_id2) {
                            stack.push(prev_id2);
                        }
                    }
                }

                // Sort blocks by time (to increase processing locality) and seqno
                blocks.sort_unstable_by_key(|(info, block_data)| (*info, block_data.id().seq_no));

                Ok::<_, anyhow::Error>(blocks)
            }));
        }

        // Wait and process all shard blocks
        for task in tasks {
            let blocks = task.await??;
            for (_, item) in blocks {
                self.process_block(item.block(), &self.sc_subscriptions)?;
            }
        }
        self.process_block(next_mc_block.block(), &self.mc_subscriptions)?;

        // Remove expired messages and empty subscriptions
        self.subscriptions_gc(&self.mc_subscriptions, next_mc_utime);
        self.subscriptions_gc(&self.sc_subscriptions, next_mc_utime);

        // Update last mc block
        let shards_edge = Edge(
            next_shard_block_ids
                .into_iter()
                .map(|(shard, id)| (shard, id.seq_no))
                .collect(),
        );

        self.last_mc_block.store(Some(Arc::new(StoredMcBlock {
            data: next_mc_block,
            shards_edge,
        })));

        // Done
        Ok(())
    }

    async fn get_last_mc_block(&self) -> Result<Arc<StoredMcBlock>> {
        // Always try to use the cached one
        if let Some(last_mc_block) = &*self.last_mc_block.load() {
            return Ok(last_mc_block.clone());
        }
        self.update_last_mc_block().await
    }

    async fn update_last_mc_block(&self) -> Result<Arc<StoredMcBlock>> {
        let stats = self.node_tcp_rpc.get_stats().await?;
        let last_mc_block = stats.try_into_running()?.last_mc_block;
        let data = self.node_udp_rpc.get_block(&last_mc_block).await?;

        let shards_edge = Edge(data.shard_blocks_seq_no()?);

        let block = Arc::new(StoredMcBlock { data, shards_edge });
        self.last_mc_block.store(Some(block.clone()));
        Ok(block)
    }

    fn process_block(
        &self,
        block: &ton_block::Block,
        subscriptions: &AccountSubscriptions,
    ) -> Result<()> {
        use ton_block::HashmapAugType;

        let counter = &self.subscription_count;
        let extra = block.read_extra()?;
        let account_blocks = extra.read_account_blocks()?;

        account_blocks.iterate_with_keys(|address, account_block| {
            let mut subscription = match subscriptions.get_mut(&address) {
                Some(subscription) if !subscription.is_empty() => subscription,
                _ => return Ok(true),
            };

            account_block
                .transactions()
                .iterate_slices_with_keys(|_, tx| {
                    let cell = tx.reference(0)?;
                    let hash = cell.repr_hash();
                    let data = ton_block::Transaction::construct_from_cell(cell)?;
                    let tx = TransactionWithHash { hash, data };

                    for channel in &subscription.transactions {
                        channel.send(tx.clone()).ok();
                    }

                    let in_msg_hash = match &tx.data.in_msg {
                        Some(in_msg) => in_msg.hash(),
                        None => return Ok(true),
                    };

                    let mut pending_message =
                        match subscription.pending_messages.remove(&in_msg_hash) {
                            Some(pending_message) => pending_message,
                            None => return Ok(true),
                        };

                    counter.fetch_sub(1, Ordering::Release);

                    if let Some(channel) = pending_message.tx.take() {
                        channel.send(Some(tx)).ok();
                    }

                    Ok(true)
                })?;

            Ok(true)
        })?;

        Ok(())
    }

    fn subscriptions_gc(&self, subscriptions: &AccountSubscriptions, utime: u32) {
        let counter = &self.subscription_count;

        subscriptions.retain(|_, subscription| {
            subscription.pending_messages.retain(|_, message| {
                let is_invalid = message.expire_at < utime;
                if is_invalid {
                    counter.fetch_sub(1, Ordering::Release);
                }
                !is_invalid
            });

            subscription.transactions.retain(|tx| {
                let is_closed = tx.is_closed();
                if is_closed {
                    counter.fetch_sub(1, Ordering::Release);
                }
                !is_closed
            });

            !subscription.is_empty()
        });
    }

    fn has_subscriptions(&self) -> bool {
        self.subscription_count.load(Ordering::Acquire) > 0
    }
}

#[derive(Default)]
struct AccountSubscription {
    pending_messages: FxHashMap<ton_types::UInt256, PendingMessage>,
    transactions: Vec<TransactionsTx>,
}

impl AccountSubscription {
    fn is_empty(&self) -> bool {
        self.pending_messages.is_empty() && self.transactions.is_empty()
    }
}

type AccountSubscriptions = FxDashMap<ton_types::UInt256, AccountSubscription>;

pub type TransactionsTx = mpsc::UnboundedSender<TransactionWithHash>;
pub type TransactionsRx = mpsc::UnboundedReceiver<TransactionWithHash>;

async fn walk_blocks(subscription: Weak<Subscription>) {
    loop {
        let subscription = match subscription.upgrade() {
            Some(subscription) => subscription,
            None => return,
        };

        let pending_messages_changed = subscription.subscriptions_changed.clone();
        let signal = pending_messages_changed.notified();

        if subscription.has_subscriptions() {
            // Update the latest masterchain block before starting the blocks loop.
            // All message senders will wait until `subscription_loop_step` is triggered.
            if let Err(e) = subscription.update_last_mc_block().await {
                tracing::error!("failed to update last mc block: {e:?}");
            }

            while subscription.has_subscriptions() {
                if let Err(e) = subscription.make_blocks_step().await {
                    tracing::error!("failed to make blocks step: {e:?}");
                }
            }
        }
        drop(subscription);

        tracing::debug!("waiting for new messages");
        signal.await;
    }
}

struct StoredMcBlock {
    data: BlockStuff,
    shards_edge: Edge,
}

struct Edge(FxHashMap<ton_block::ShardIdent, u32>);

impl Edge {
    pub fn is_before(&self, id: &ton_block::BlockIdExt) -> bool {
        match self.0.get(&id.shard_id) {
            Some(&top_seq_no) => top_seq_no < id.seq_no,
            None => self
                .0
                .iter()
                .find(|&(shard, _)| id.shard_id.intersect_with(shard))
                .map(|(_, &top_seq_no)| top_seq_no < id.seq_no)
                .unwrap_or_default(),
        }
    }
}

struct PendingMessage {
    expire_at: u32,
    tx: Option<oneshot::Sender<Option<TransactionWithHash>>>,
}

impl Drop for PendingMessage {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            tx.send(None).ok();
        }
    }
}

fn requires_signature_id(capabilities: u64) -> bool {
    const CAP_WITH_SIGNATURE_ID: u64 = 0x4000000;

    capabilities & CAP_WITH_SIGNATURE_ID != 0
}
