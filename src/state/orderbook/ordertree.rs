use anchor_lang::prelude::*;
use bytemuck::{cast, cast_mut, cast_ref};

use num_enum::{IntoPrimitive, TryFromPrimitive};

use super::*;
use crate::error::OpenBookError;

pub const MAX_ORDERTREE_NODES: usize = 1024;

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
pub enum OrderTreeType {
    Bids,
    Asks,
}

impl OrderTreeType {
    pub fn side(&self) -> Side {
        match *self {
            Self::Bids => Side::Bid,
            Self::Asks => Side::Ask,
        }
    }
}

#[zero_copy]
#[derive(Debug, anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)]
pub struct OrderTreeRoot {
    pub maybe_node: NodeHandle,
    pub leaf_count: u32,
}

impl OrderTreeRoot {
    pub fn node(&self) -> Option<NodeHandle> {
        if self.leaf_count == 0 {
            None
        } else {
            Some(self.maybe_node)
        }
    }
}

/// A binary tree on AnyNode::key()
///
/// The key encodes the price in the top 64 bits.
#[derive(Debug, Clone, Copy, anchor_lang::AnchorSerialize, anchor_lang::AnchorDeserialize)]
pub struct OrderTreeNodes {
    pub order_tree_type: u8, // OrderTreeType, but that's not POD
    pub padding: [u8; 3],
    pub bump_index: u32,
    pub free_list_len: u32,
    pub free_list_head: NodeHandle,
    pub reserved: [u8; 512],
    pub nodes: [AnyNode; MAX_ORDERTREE_NODES],
}

impl OrderTreeNodes {
    pub fn order_tree_type(&self) -> OrderTreeType {
        OrderTreeType::try_from(self.order_tree_type).unwrap()
    }

    /// Iterate over all entries, including invalid orders
    ///
    /// smallest to highest for asks
    /// highest to smallest for bids
    pub fn iter(&self, root: &OrderTreeRoot) -> OrderTreeIter {
        OrderTreeIter::new(self, root)
    }

    pub fn node_mut(&mut self, handle: NodeHandle) -> Option<&mut AnyNode> {
        let node = &mut self.nodes[handle as usize];
        let tag = NodeTag::try_from(node.tag);
        match tag {
            Ok(NodeTag::InnerNode) | Ok(NodeTag::LeafNode) => Some(node),
            _ => None,
        }
    }
    pub fn node(&self, handle: NodeHandle) -> Option<&AnyNode> {
        let node = &self.nodes[handle as usize];
        let tag = NodeTag::try_from(node.tag);
        match tag {
            Ok(NodeTag::InnerNode) | Ok(NodeTag::LeafNode) => Some(node),
            _ => None,
        }
    }

    pub fn remove_worst(&mut self, root: &mut OrderTreeRoot) -> Option<LeafNode> {
        self.remove_by_key(root, self.find_worst(root)?.1.key.into())
    }

    pub fn find_worst(&self, root: &OrderTreeRoot) -> Option<(NodeHandle, &LeafNode)> {
        match self.order_tree_type() {
            OrderTreeType::Bids => self.min_leaf(root),
            OrderTreeType::Asks => self.max_leaf(root),
        }
    }

    /// Remove the order with the lowest expiry timestamp, if that's < now_ts.
    pub fn remove_one_expired(
        &mut self,
        root: &mut OrderTreeRoot,
        now_ts: u64,
    ) -> Option<LeafNode> {
        let (handle, expires_at) = self.find_earliest_expiry(root)?;
        if expires_at < now_ts {
            self.remove_by_key(root, self.node(handle)?.key()?)
        } else {
            None
        }
    }

    // only for fixed-price ordertrees
    #[cfg(test)]
    #[allow(dead_code)]
    fn as_price_quantity_vec(&self, root: &OrderTreeRoot, reverse: bool) -> Vec<(i64, i64)> {
        let mut pqs = vec![];
        let mut current: NodeHandle = match root.node() {
            None => return pqs,
            Some(node_handle) => node_handle,
        };

        let left = reverse as usize;
        let right = !reverse as usize;
        let mut stack = vec![];
        loop {
            let root_contents = self.node(current).unwrap(); // should never fail unless book is already fucked
            match root_contents.case().unwrap() {
                NodeRef::Inner(inner) => {
                    stack.push(inner);
                    current = inner.children[left];
                }
                NodeRef::Leaf(leaf) => {
                    // if you hit leaf then pop stack and go right
                    // all inner nodes on stack have already been visited to the left
                    pqs.push((fixed_price_lots(leaf.price_data()), leaf.quantity));
                    match stack.pop() {
                        None => return pqs,
                        Some(inner) => {
                            current = inner.children[right];
                        }
                    }
                }
            }
        }
    }

    pub fn min_leaf(&self, root: &OrderTreeRoot) -> Option<(NodeHandle, &LeafNode)> {
        self.leaf_min_max(false, root)
    }

    pub fn max_leaf(&self, root: &OrderTreeRoot) -> Option<(NodeHandle, &LeafNode)> {
        self.leaf_min_max(true, root)
    }
    fn leaf_min_max(
        &self,
        find_max: bool,
        root: &OrderTreeRoot,
    ) -> Option<(NodeHandle, &LeafNode)> {
        let mut node_handle: NodeHandle = root.node()?;

        let i = usize::from(find_max);
        loop {
            let node_contents = self.node(node_handle)?;
            match node_contents.case()? {
                NodeRef::Inner(inner) => {
                    node_handle = inner.children[i];
                }
                NodeRef::Leaf(leaf) => {
                    return Some((node_handle, leaf));
                }
            }
        }
    }

    pub fn remove_by_key(
        &mut self,
        root: &mut OrderTreeRoot,
        search_key: u128,
    ) -> Option<LeafNode> {
        // path of InnerNode handles that lead to the removed leaf
        let mut stack: Vec<(NodeHandle, bool)> = vec![];

        // special case potentially removing the root
        let mut parent_h = root.node()?;
        let (mut child_h, mut crit_bit) = match self.node(parent_h).unwrap().case().unwrap() {
            NodeRef::Leaf(&leaf) if u128::from(leaf.key) == search_key => {
                assert_eq!(root.leaf_count, 1);
                root.maybe_node = 0;
                root.leaf_count = 0;
                let _old_root = self.remove(parent_h).unwrap();
                return Some(leaf);
            }
            NodeRef::Leaf(_) => return None,
            NodeRef::Inner(inner) => inner.walk_down(search_key),
        };
        stack.push((parent_h, crit_bit));

        // walk down the tree until finding the key
        loop {
            match self.node(child_h).unwrap().case().unwrap() {
                NodeRef::Inner(inner) => {
                    parent_h = child_h;
                    let (new_child_h, new_crit_bit) = inner.walk_down(search_key);
                    child_h = new_child_h;
                    crit_bit = new_crit_bit;
                    stack.push((parent_h, crit_bit));
                }
                NodeRef::Leaf(leaf) => {
                    if u128::from(leaf.key) != search_key {
                        return None;
                    }
                    break;
                }
            }
        }

        // replace parent with its remaining child node
        // free child_h, replace *parent_h with *other_child_h, free other_child_h
        let other_child_h = self.node(parent_h).unwrap().children().unwrap()[!crit_bit as usize];
        let other_child_node_contents = self.remove(other_child_h).unwrap();
        let new_expiry = other_child_node_contents.earliest_expiry();
        *self.node_mut(parent_h).unwrap() = other_child_node_contents;
        root.leaf_count -= 1;
        let removed_leaf: LeafNode = cast(self.remove(child_h).unwrap());

        // update child min expiry back up to the root
        let outdated_expiry = removed_leaf.expiry();
        stack.pop(); // the final parent has been replaced by the remaining leaf
        self.update_parent_earliest_expiry(&stack, outdated_expiry, new_expiry);

        Some(removed_leaf)
    }

    /// Internal: Removes only the node, does not remove any links etc, use remove_key()
    fn remove(&mut self, key: NodeHandle) -> Option<AnyNode> {
        let val = *self.node(key)?;

        self.nodes[key as usize] = cast(FreeNode {
            tag: if self.free_list_len == 0 {
                NodeTag::LastFreeNode.into()
            } else {
                NodeTag::FreeNode.into()
            },
            padding: Default::default(),
            next: self.free_list_head,
            reserved: [0; 72],
            force_align: 0,
        });

        self.free_list_len += 1;
        self.free_list_head = key;
        Some(val)
    }

    /// Internal: Adds only the node, does not add parent links etc, use insert_leaf()
    fn insert(&mut self, val: &AnyNode) -> Result<NodeHandle> {
        match NodeTag::try_from(val.tag) {
            Ok(NodeTag::InnerNode) | Ok(NodeTag::LeafNode) => (),
            _ => unreachable!(),
        };

        if self.free_list_len == 0 {
            require!(
                (self.bump_index as usize) < self.nodes.len() && self.bump_index < u32::MAX,
                OpenBookError::SomeError
            );

            self.nodes[self.bump_index as usize] = *val;
            let key = self.bump_index;
            self.bump_index += 1;
            return Ok(key);
        }

        let key = self.free_list_head;
        let node = &mut self.nodes[key as usize];

        match NodeTag::try_from(node.tag) {
            Ok(NodeTag::FreeNode) => assert!(self.free_list_len > 1),
            Ok(NodeTag::LastFreeNode) => assert_eq!(self.free_list_len, 1),
            _ => unreachable!(),
        };

        self.free_list_head = cast_ref::<AnyNode, FreeNode>(node).next;
        self.free_list_len -= 1;
        *node = *val;
        Ok(key)
    }

    pub fn insert_leaf(
        &mut self,
        root: &mut OrderTreeRoot,
        new_leaf: &LeafNode,
    ) -> Result<(NodeHandle, Option<LeafNode>)> {
        // path of InnerNode handles that lead to the new leaf
        let mut stack: Vec<(NodeHandle, bool)> = vec![];

        // deal with inserts into an empty tree
        let mut parent_handle: NodeHandle = match root.node() {
            Some(h) => h,
            None => {
                // create a new root if none exists
                let handle = self.insert(new_leaf.as_ref())?;
                root.maybe_node = handle;
                root.leaf_count = 1;
                return Ok((handle, None));
            }
        };

        // walk down the tree until we find the insert location
        loop {
            // require if the new node will be a child of the root
            let parent_contents = *self.node(parent_handle).unwrap();
            let parent_key = parent_contents.key().unwrap();
            if parent_key == u128::from(new_leaf.key) {
                // This should never happen because key should never match
                if let Some(NodeRef::Leaf(&old_parent_as_leaf)) = parent_contents.case() {
                    // clobber the existing leaf
                    *self.node_mut(parent_handle).unwrap() = *new_leaf.as_ref();
                    self.update_parent_earliest_expiry(
                        &stack,
                        old_parent_as_leaf.expiry(),
                        new_leaf.expiry(),
                    );
                    return Ok((parent_handle, Some(old_parent_as_leaf)));
                }
                // InnerNodes have a random child's key, so matching can happen and is fine
            }
            let shared_prefix_len: u32 = (parent_key ^  u128::from(new_leaf.key)).leading_zeros();
            match parent_contents.case() {
                None => unreachable!(),
                Some(NodeRef::Inner(inner)) => {
                    let keep_old_parent = shared_prefix_len >= inner.prefix_len;
                    if keep_old_parent {
                        let (child, crit_bit) = inner.walk_down(new_leaf.key.into());
                        stack.push((parent_handle, crit_bit));
                        parent_handle = child;
                        continue;
                    };
                }
                _ => (),
            };
            // implies parent is a Leaf or Inner where shared_prefix_len < prefix_len
            // we'll replace parent with a new InnerNode that has new_leaf and parent as children

            // change the parent in place to represent the LCA of [new_leaf] and [parent]
            let crit_bit_mask: u128 = 1u128 << (127 - shared_prefix_len);
            let new_leaf_crit_bit = (crit_bit_mask & u128::from(new_leaf.key)) != 0;
            let old_parent_crit_bit = !new_leaf_crit_bit;

            let new_leaf_handle = self.insert(new_leaf.as_ref())?;
            let moved_parent_handle = match self.insert(&parent_contents) {
                Ok(h) => h,
                Err(e) => {
                    self.remove(new_leaf_handle).unwrap();
                    return Err(e);
                }
            };

            let new_parent: &mut InnerNode = cast_mut(self.node_mut(parent_handle).unwrap());
            *new_parent = InnerNode::new(shared_prefix_len, new_leaf.key.into());

            new_parent.children[new_leaf_crit_bit as usize] = new_leaf_handle;
            new_parent.children[old_parent_crit_bit as usize] = moved_parent_handle;

            let new_leaf_expiry = new_leaf.expiry();
            let old_parent_expiry = parent_contents.earliest_expiry();
            new_parent.child_earliest_expiry[new_leaf_crit_bit as usize] = new_leaf_expiry;
            new_parent.child_earliest_expiry[old_parent_crit_bit as usize] = old_parent_expiry;

            // walk up the stack and fix up the new min if needed
            if new_leaf_expiry < old_parent_expiry {
                self.update_parent_earliest_expiry(&stack, old_parent_expiry, new_leaf_expiry);
            }

            root.leaf_count += 1;
            return Ok((new_leaf_handle, None));
        }
    }

    pub fn is_full(&self) -> bool {
        self.free_list_len <= 1 && (self.bump_index as usize) >= self.nodes.len() - 1
    }

    /// When a node changes, the parents' child_earliest_expiry may need to be updated.
    ///
    /// This function walks up the `stack` of parents and applies the change where the
    /// previous child's `outdated_expiry` is replaced by `new_expiry`.
    pub fn update_parent_earliest_expiry(
        &mut self,
        stack: &[(NodeHandle, bool)],
        mut outdated_expiry: u64,
        mut new_expiry: u64,
    ) {
        // Walk from the top of the stack to the root of the tree.
        // Since the stack grows by appending, we need to iterate the slice in reverse order.
        for (parent_h, crit_bit) in stack.iter().rev() {
            let parent = self.node_mut(*parent_h).unwrap().as_inner_mut().unwrap();
            if parent.child_earliest_expiry[*crit_bit as usize] != outdated_expiry {
                break;
            }
            outdated_expiry = parent.earliest_expiry();
            parent.child_earliest_expiry[*crit_bit as usize] = new_expiry;
            new_expiry = parent.earliest_expiry();
        }
    }

    /// Returns the handle of the node with the lowest expiry timestamp, and this timestamp
    pub fn find_earliest_expiry(&self, root: &OrderTreeRoot) -> Option<(NodeHandle, u64)> {
        let mut current: NodeHandle = root.node()?;

        loop {
            let contents = *self.node(current).unwrap();
            match contents.case() {
                None => unreachable!(),
                Some(NodeRef::Inner(inner)) => {
                    current = inner.children[(inner.child_earliest_expiry[0]
                        > inner.child_earliest_expiry[1])
                        as usize];
                }
                _ => {
                    return Some((current, contents.earliest_expiry()));
                }
            };
        }
    }
}
