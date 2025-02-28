use anchor_lang::prelude::*;
use num_enum::{IntoPrimitive, TryFromPrimitive};

use super::*;

#[derive(
    Eq,
    PartialEq,
    Copy,
    Clone,
    TryFromPrimitive,
    IntoPrimitive,
    Debug,
    AnchorSerialize,
    AnchorDeserialize,
)]
#[repr(u8)]
pub enum BookSideOrderTree {
    Fixed = 0,
    OraclePegged = 1,
}

/// Reference to a node in a book side component
pub struct BookSideOrderHandle {
    pub node: NodeHandle,
    pub order_tree: BookSideOrderTree,
}

#[derive(Debug, anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)]
pub struct BookSide {
    pub roots: [OrderTreeRoot; 2],
    pub reserved_roots: [OrderTreeRoot; 4],
    pub reserved: [u8; 256],
    pub nodes: OrderTreeNodes,
}

impl BookSide {
    /// The length of the BookSide account
    pub const LEN: usize = 8 + std::mem::size_of::<Self>();

    /// Iterate over all entries in the book filtering out invalid orders
    ///
    /// smallest to highest for asks
    /// highest to smallest for bids
    pub fn iter_valid(
        &self,
        now_ts: u64,
        oracle_price_lots: Option<i64>,
    ) -> impl Iterator<Item = BookSideIterItem> {
        BookSideIter::new(self, now_ts, oracle_price_lots).filter(|it| it.is_valid())
    }

    /// Iterate over all entries, including invalid orders
    pub fn iter_all_including_invalid(
        &self,
        now_ts: u64,
        oracle_price_lots: Option<i64>,
    ) -> BookSideIter {
        BookSideIter::new(self, now_ts, oracle_price_lots)
    }

    pub fn node(&self, handle: NodeHandle) -> Option<&AnyNode> {
        self.nodes.node(handle)
    }

    pub fn node_mut(&mut self, handle: NodeHandle) -> Option<&mut AnyNode> {
        self.nodes.node_mut(handle)
    }

    pub fn root(&self, component: BookSideOrderTree) -> &OrderTreeRoot {
        &self.roots[component as usize]
    }

    pub fn root_mut(&mut self, component: BookSideOrderTree) -> &mut OrderTreeRoot {
        &mut self.roots[component as usize]
    }

    pub fn is_full(&self) -> bool {
        self.nodes.is_full()
    }

    pub fn is_empty(&self) -> bool {
        [BookSideOrderTree::Fixed, BookSideOrderTree::OraclePegged]
            .into_iter()
            .all(|component| self.nodes.iter(self.root(component)).count() == 0)
    }

    pub fn insert_leaf(
        &mut self,
        component: BookSideOrderTree,
        new_leaf: &LeafNode,
    ) -> Result<(NodeHandle, Option<LeafNode>)> {
        let root = &mut self.roots[component as usize];
        self.nodes.insert_leaf(root, new_leaf)
    }

    /// Remove the overall worst-price order.
    pub fn remove_worst(
        &mut self,
        now_ts: u64,
        oracle_price_lots: Option<i64>,
    ) -> Option<(LeafNode, i64)> {
        let worst_fixed = self.nodes.find_worst(&self.roots[0]);
        let worst_pegged = self.nodes.find_worst(&self.roots[1]);
        let side = self.nodes.order_tree_type().side();
        let worse = rank_orders(
            side,
            worst_fixed,
            worst_pegged,
            true,
            now_ts,
            oracle_price_lots,
        )?;
        let price = worse.price_lots;
        let key = worse.node.key;
        let order_tree = worse.handle.order_tree;
        let n = self.remove_by_key(order_tree, key.into())?;
        Some((n, price))
    }

    /// Remove the order with the lowest expiry timestamp in the component, if that's < now_ts.
    /// If there is none, try to remove the lowest expiry one from the other component.
    pub fn remove_one_expired(
        &mut self,
        component: BookSideOrderTree,
        now_ts: u64,
    ) -> Option<LeafNode> {
        let root = &mut self.roots[component as usize];
        if let Some(n) = self.nodes.remove_one_expired(root, now_ts) {
            return Some(n);
        }

        let other_component = match component {
            BookSideOrderTree::Fixed => BookSideOrderTree::OraclePegged,
            BookSideOrderTree::OraclePegged => BookSideOrderTree::Fixed,
        };
        let other_root = &mut self.roots[other_component as usize];
        self.nodes.remove_one_expired(other_root, now_ts)
    }

    pub fn remove_by_key(
        &mut self,
        component: BookSideOrderTree,
        search_key: u128,
    ) -> Option<LeafNode> {
        let root = &mut self.roots[component as usize];
        self.nodes.remove_by_key(root, search_key)
    }

    pub fn side(&self) -> Side {
        self.nodes.order_tree_type().side()
    }

    /// Return the quantity of orders that can be matched by an order at `limit_price_lots`
    pub fn quantity_at_price(
        &self,
        limit_price_lots: i64,
        now_ts: u64,
        oracle_price_lots: i64,
    ) -> i64 {
        let side = self.side();
        let mut sum = 0;
        for item in self.iter_valid(now_ts, Some(oracle_price_lots)) {
            if side.is_price_better(limit_price_lots, item.price_lots) {
                break;
            }
            sum += item.node.quantity;
        }
        sum
    }

    /// Return the price of the order closest to the spread
    pub fn best_price(&self, now_ts: u64, oracle_price_lots: Option<i64>) -> Option<i64> {
        Some(
            self.iter_valid(now_ts, oracle_price_lots)
                .next()?
                .price_lots,
        )
    }

    /// Walk up the book `quantity` units and return the price at that level. If `quantity` units
    /// not on book, return None
    pub fn impact_price(&self, quantity: i64, now_ts: u64, oracle_price_lots: i64) -> Option<i64> {
        let mut sum: i64 = 0;
        for order in self.iter_valid(now_ts, Some(oracle_price_lots)) {
            sum += order.node.quantity;
            if sum >= quantity {
                return Some(order.price_lots);
            }
        }
        None
    }
}
