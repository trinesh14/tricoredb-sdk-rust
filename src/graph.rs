//! Graphs: labelled nodes, typed edges, traversal, paths and Cypher.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::client::Client;
use crate::error::{Error, Result};

/// Which edges of a node an operation follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphDirection {
    /// Edges the node is the `from` end of. The server's default.
    #[default]
    Outgoing,
    /// Edges the node is the `to` end of.
    Incoming,
    /// Edges in either direction.
    Both,
}

/// One stored node.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphNode {
    /// The node's id, unique within its graph.
    pub id: String,
    /// Its labels.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub labels: Vec<String>,
    /// Its properties.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub properties: Map<String, Value>,
}

/// One stored edge, directed from one node to another.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphEdge {
    /// The edge's id, unique within its graph.
    pub id: String,
    /// The node it leaves.
    #[serde(default)]
    pub from: String,
    /// The node it enters.
    #[serde(default)]
    pub to: String,
    /// Its label.
    #[serde(default)]
    pub label: String,
    /// Its properties.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub properties: Map<String, Value>,
}

/// One edge incident to a node, and the node at its other end.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphNeighbor {
    /// The incident edge.
    pub edge_id: String,
    /// The node at the far end.
    pub node_id: String,
    /// The edge's label.
    #[serde(default)]
    pub label: String,
    /// The edge's orientation relative to the node that was asked about, which
    /// is what makes a `Both` result readable.
    #[serde(default)]
    pub direction: GraphDirection,
}

/// One node reached by a traversal.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphVisit {
    /// The node's id.
    pub id: String,
    /// How many hops away it was first reached.
    #[serde(default)]
    pub depth: usize,
    /// Its labels.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub labels: Vec<String>,
    /// Its properties.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub properties: Map<String, Value>,
}

/// The outcome of a bounded breadth-first walk.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphTraversal {
    /// The node the walk began at.
    #[serde(default)]
    pub start: String,
    /// The direction that was followed.
    #[serde(default)]
    pub direction: String,
    /// The hop bound the server applied.
    #[serde(default)]
    pub max_depth: usize,
    /// The node bound the server applied.
    #[serde(default)]
    pub limit: usize,
    /// How many nodes came back.
    #[serde(default)]
    pub count: usize,
    /// Whether a bound stopped the walk before it ran out of nodes. The nodes
    /// returned are real; the answer is simply not exhaustive.
    #[serde(default)]
    pub truncated: bool,
    /// The visited nodes, in breadth-first order.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub nodes: Vec<GraphVisit>,
}

/// The outcome of a path search.
///
/// `found == false` is an ordinary answer, not an error: either no path exists
/// or the search stopped at a bound, and [`GraphPath::message`] says which. A
/// search stopped by a bound is inconclusive, not proof that no path exists.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphPath {
    /// Whether a path was found.
    #[serde(default)]
    pub found: bool,
    /// The start node.
    #[serde(default)]
    pub from: String,
    /// The target node.
    #[serde(default)]
    pub to: String,
    /// The direction that was followed.
    #[serde(default)]
    pub direction: String,
    /// How many edges the path has.
    #[serde(default)]
    pub hops: usize,
    /// The node ids along the path, the start first.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub node_path: Vec<String>,
    /// The edge ids along the path.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub edge_path: Vec<String>,
    /// The summed weight — meaningful only for the weighted search, and zero
    /// for the unweighted one, which minimises hops.
    #[serde(default)]
    pub total_cost: f64,
    /// Why no path was found, when none was.
    #[serde(default)]
    pub message: String,
}

/// One page of a graph's nodes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphNodePage {
    /// The graph's name.
    #[serde(default)]
    pub graph: String,
    /// How many nodes are on this page.
    #[serde(default)]
    pub count: usize,
    /// This page's nodes.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub nodes: Vec<GraphNode>,
    /// Whether more nodes remain past this page.
    #[serde(default)]
    pub truncated: bool,
    /// The graph's full node count.
    #[serde(default)]
    pub total: i64,
}

/// One page of a graph's edges.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphEdgePage {
    /// The graph's name.
    #[serde(default)]
    pub graph: String,
    /// How many edges are on this page.
    #[serde(default)]
    pub count: usize,
    /// This page's edges.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub edges: Vec<GraphEdge>,
    /// Whether more edges remain past this page.
    #[serde(default)]
    pub truncated: bool,
    /// The graph's full edge count.
    #[serde(default)]
    pub total: i64,
}

/// The result of a Cypher query.
///
/// Cells stay as JSON because a `RETURN` can yield a scalar, a list, or a whole
/// node.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphRows {
    /// The graph that was queried.
    #[serde(default)]
    pub graph: String,
    /// The `RETURN` column names.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub columns: Vec<String>,
    /// The result rows.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub rows: Vec<Vec<Value>>,
    /// How many rows came back.
    #[serde(default)]
    pub count: usize,
    /// Whether the server's row cap cut the result short.
    #[serde(default)]
    pub truncated: bool,
}

/// What [`Client::graph_neighbors`] should include. The default follows
/// outgoing edges of every label, up to the server's cap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NeighborOptions {
    /// Which edges to follow. `None` leaves it to the server (outgoing).
    pub direction: Option<GraphDirection>,
    /// Keep only edges with this label. `None` means every label.
    pub label: Option<String>,
    /// Cap the neighbors returned. `None` asks for the server default.
    pub limit: Option<usize>,
}

/// What [`Client::graph_traverse`] should walk. The default walks outgoing
/// edges of every label to the server's default depth and node limit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraverseOptions {
    /// Which edges to follow.
    pub direction: Option<GraphDirection>,
    /// Keep only edges with this label.
    pub label: Option<String>,
    /// Bound the hop count. The server's default is 3 and it clamps above 10.
    pub max_depth: Option<usize>,
    /// Cap the nodes returned. The server's default is 100, clamped to 1000.
    pub limit: Option<usize>,
}

/// What [`Client::graph_shortest_path`] should search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathOptions {
    /// Which edges to follow.
    pub direction: Option<GraphDirection>,
    /// Keep only edges with this label.
    pub label: Option<String>,
    /// Bound the hop count. The server's default is also its maximum, 10.
    pub max_depth: Option<usize>,
}

/// What [`Client::graph_weighted_shortest_path`] should search.
///
/// There is deliberately no depth bound: a weighted search is bounded by cost,
/// not by hops, and a depth clamp would discard the cheap-but-long path that is
/// the whole reason to run one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WeightedPathOptions {
    /// Which edges to follow.
    pub direction: Option<GraphDirection>,
    /// Keep only edges with this label.
    pub label: Option<String>,
    /// The edge property holding the cost. `None` means `"weight"`. An edge
    /// missing it, or holding something that is not a number, weighs 1.0; a
    /// negative weight is refused rather than solved wrongly.
    pub weight_property: Option<String>,
}

/// How [`Client::graph_connected_components`] groups nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    /// Ignore edge direction. The default.
    #[default]
    Weak,
    /// Group nodes that can each reach the other along edge direction.
    Strong,
}

/// The rule a constraint enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    /// No two records with the label share a value for the property.
    Unique,
    /// Every record with the label has the property.
    Required,
}

/// What a constraint applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintTarget {
    /// Nodes carrying the label. The default.
    #[default]
    Node,
    /// Relationships of that type.
    Edge,
}

/// The measure [`Client::graph_centrality`] computes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CentralityMeasure {
    /// How often a node lies on a shortest path between two others.
    Betweenness,
    /// How near a node is to every other.
    Closeness,
    /// How well connected a node's neighbours are.
    Eigenvector,
}

/// How [`Client::graph_node_similarity`] compares neighbour sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimilarityMetric {
    /// Shared neighbours over all neighbours. The default.
    #[default]
    Jaccard,
    /// Shared neighbours over the smaller set.
    Overlap,
}

/// One node of [`Client::graph_add_many`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphNodeInput {
    /// The node's id.
    pub id: String,
    /// Its labels.
    pub labels: Vec<String>,
    /// Its properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Map<String, Value>>,
}

impl GraphNodeInput {
    /// A node with these labels and no properties.
    pub fn new(id: &str, labels: &[&str]) -> Self {
        Self {
            id: id.to_string(),
            labels: labels.iter().map(|l| l.to_string()).collect(),
            properties: None,
        }
    }

    /// Attach properties.
    pub fn properties(mut self, properties: Map<String, Value>) -> Self {
        self.properties = Some(properties);
        self
    }
}

/// One edge of [`Client::graph_add_many`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphEdgeInput {
    /// The edge's id.
    pub id: String,
    /// The node it leaves.
    pub from: String,
    /// The node it enters.
    pub to: String,
    /// Its label.
    pub label: String,
    /// Its properties.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Map<String, Value>>,
}

impl GraphEdgeInput {
    /// An edge with no properties.
    pub fn new(id: &str, from: &str, to: &str, label: &str) -> Self {
        Self {
            id: id.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            label: label.to_string(),
            properties: None,
        }
    }

    /// Attach properties.
    pub fn properties(mut self, properties: Map<String, Value>) -> Self {
        self.properties = Some(properties);
        self
    }
}

/// How many nodes and edges a batch write or a copy handled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct GraphCounts {
    /// How many nodes.
    #[serde(default)]
    pub nodes: i64,
    /// How many edges.
    #[serde(default)]
    pub edges: i64,
}

/// What [`Client::graph_remove_many`] deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct GraphRemoved {
    /// How many nodes went.
    #[serde(default)]
    pub nodes_removed: i64,
    /// How many edges went, those detached with a node included.
    #[serde(default)]
    pub edges_removed: i64,
}

/// What [`Client::graph_update_node`] should change. The default changes
/// nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeUpdate {
    /// A JSON merge patch for the properties: a key set to a value is set, a
    /// key set to `null` is removed, and a key left out is left alone.
    pub properties: Option<Value>,
    /// Labels to add. Adding one the node already has is a no-op.
    pub add_labels: Vec<String>,
    /// Labels to remove, applied before the additions.
    pub remove_labels: Vec<String>,
}

/// What [`Client::graph_connected_components`] should consider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComponentOptions {
    /// Weak or strong connectivity. `None` leaves it to the server (weak).
    pub kind: Option<ComponentKind>,
    /// Consider only edges with this label.
    pub label: Option<String>,
    /// Cap the components listed, never the number counted. The server's
    /// default is 100, capped at 1000.
    pub limit: Option<usize>,
}

/// One group of nodes: a connected component, or a community.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphGroup {
    /// The ids of its nodes.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// How many nodes it has, which may be more than are listed.
    #[serde(default)]
    pub size: usize,
    /// Whether the node list was cut short.
    #[serde(default)]
    pub truncated: bool,
}

/// The connected components of a graph, largest first.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphComponents {
    /// The connectivity that was computed.
    #[serde(default)]
    pub kind: ComponentKind,
    /// How many components the graph has.
    #[serde(default)]
    pub count: usize,
    /// The size of the largest.
    #[serde(default)]
    pub largest: usize,
    /// Whether more components exist than are listed.
    #[serde(default)]
    pub truncated: bool,
    /// The components listed.
    #[serde(default)]
    pub components: Vec<GraphGroup>,
}

/// One cycle, as the evidence [`Client::graph_detect_cycle`] returns.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphCycle {
    /// The node ids around the cycle.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// The edge ids around the cycle.
    #[serde(default)]
    pub edges: Vec<String>,
}

/// The answer to [`Client::graph_detect_cycle`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphCycleCheck {
    /// Whether the graph contains a cycle.
    #[serde(default, rename = "hasCycle")]
    pub has_cycle: bool,
    /// Whether edge direction was followed.
    #[serde(default)]
    pub directed: bool,
    /// One cycle, when there is one.
    #[serde(default)]
    pub cycle: Option<GraphCycle>,
}

/// Counts describing a graph.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStats {
    /// How many nodes.
    #[serde(default)]
    pub nodes: i64,
    /// How many edges.
    #[serde(default)]
    pub edges: i64,
    /// Nodes per label.
    #[serde(default)]
    pub node_labels: BTreeMap<String, i64>,
    /// Edges per type.
    #[serde(default)]
    pub edge_types: BTreeMap<String, i64>,
    /// Nodes with no label.
    #[serde(default)]
    pub unlabeled_nodes: i64,
    /// Edges from a node to itself.
    #[serde(default)]
    pub self_loops: i64,
    /// Nodes with no edge.
    #[serde(default)]
    pub isolated_nodes: i64,
    /// The mean number of outgoing edges per node.
    #[serde(default)]
    pub avg_out_degree: f64,
    /// The most outgoing edges any node has.
    #[serde(default)]
    pub max_out_degree: i64,
    /// The most incoming edges any node has.
    #[serde(default)]
    pub max_in_degree: i64,
    /// Edges whose endpoint no longer exists.
    #[serde(default)]
    pub dangling_edges: i64,
}

/// The part of a graph around some seed nodes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphSubgraph {
    /// The direction that was followed.
    #[serde(default)]
    pub direction: String,
    /// The hop bound the server applied.
    #[serde(default)]
    pub max_depth: usize,
    /// The node bound the server applied.
    #[serde(default)]
    pub limit: usize,
    /// How many nodes came back.
    #[serde(default)]
    pub node_count: usize,
    /// How many edges came back.
    #[serde(default)]
    pub edge_count: usize,
    /// The nodes reached, each with the depth it was reached at.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub nodes: Vec<GraphVisit>,
    /// Every edge joining two of those nodes.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub edges: Vec<GraphEdge>,
    /// Whether the node bound cut the walk short.
    #[serde(default)]
    pub nodes_truncated: bool,
    /// Whether the edge list was cut short.
    #[serde(default)]
    pub edges_truncated: bool,
}

/// One property key seen on a label or an edge type.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphPropertySchema {
    /// The property key.
    pub key: String,
    /// How many records carry it.
    #[serde(default)]
    pub count: i64,
    /// The JSON types seen under it.
    #[serde(default)]
    pub types: Vec<String>,
}

/// One node label, or the unlabelled nodes, in a [`GraphSchema`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct GraphLabelSchema {
    /// The label. Empty for the unlabelled nodes.
    #[serde(default)]
    pub label: String,
    /// How many nodes carry it.
    #[serde(default)]
    pub count: i64,
    /// The property keys seen on them.
    #[serde(default)]
    pub properties: Vec<GraphPropertySchema>,
}

/// One edge type in a [`GraphSchema`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphEdgeTypeSchema {
    /// The edge type.
    #[serde(rename = "type")]
    pub edge_type: String,
    /// How many edges have it.
    #[serde(default)]
    pub count: i64,
    /// The property keys seen on them.
    #[serde(default)]
    pub properties: Vec<GraphPropertySchema>,
}

/// What a graph's data looks like.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphSchema {
    /// How many nodes the graph holds.
    #[serde(default)]
    pub nodes: i64,
    /// How many edges the graph holds.
    #[serde(default)]
    pub edges: i64,
    /// Each node label.
    #[serde(default)]
    pub node_labels: Vec<GraphLabelSchema>,
    /// Each edge type.
    #[serde(default)]
    pub edge_types: Vec<GraphEdgeTypeSchema>,
    /// The nodes that carry no label.
    #[serde(default)]
    pub unlabeled_nodes: GraphLabelSchema,
}

/// What [`Client::graph_k_shortest_paths`] should search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KPathOptions {
    /// How many paths to return. The server's default is 3, capped at 20.
    pub k: Option<usize>,
    /// Which edges to follow.
    pub direction: Option<GraphDirection>,
    /// Keep only edges with this label.
    pub label: Option<String>,
    /// The edge property holding the cost. Without it every edge costs 1, so
    /// paths are ranked by hops.
    pub weight_property: Option<String>,
}

/// One path of a [`GraphPaths`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphPathEntry {
    /// The node ids along the path, the start first.
    #[serde(default)]
    pub nodes: Vec<String>,
    /// The edge ids along the path.
    #[serde(default)]
    pub edges: Vec<String>,
    /// How many edges the path has.
    #[serde(default)]
    pub hops: usize,
    /// The summed cost of its edges.
    #[serde(default)]
    pub cost: f64,
}

/// Several paths between two nodes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphPaths {
    /// The start node.
    #[serde(default)]
    pub from: String,
    /// The target node.
    #[serde(default)]
    pub to: String,
    /// The direction that was followed.
    #[serde(default)]
    pub direction: String,
    /// How many paths came back.
    #[serde(default)]
    pub count: usize,
    /// The paths.
    #[serde(default)]
    pub paths: Vec<GraphPathEntry>,
    /// For [`Client::graph_all_paths`], whether the enumeration finished.
    /// `None` from a search that does not report it.
    #[serde(default)]
    pub complete: Option<bool>,
    /// What stopped an enumeration that did not finish.
    #[serde(default)]
    pub stopped_by: Option<String>,
}

/// What [`Client::graph_centrality`] should compute over.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CentralityOptions {
    /// Follow edge direction. `None` leaves it to the server, which does.
    pub directed: Option<bool>,
    /// Consider only edges with this label.
    pub label: Option<String>,
    /// How many nodes to return. The server's default is 20, capped at 1000.
    pub limit: Option<usize>,
    /// The iteration bound. Eigenvector only.
    pub iterations: Option<usize>,
    /// The change below which iteration stops. Eigenvector only.
    pub tolerance: Option<f64>,
}

/// One node and the score an algorithm gave it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphScore {
    /// The node's id.
    pub id: String,
    /// Its score.
    #[serde(default)]
    pub score: f64,
}

/// The answer to [`Client::graph_centrality`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphCentrality {
    /// The measure that was computed.
    pub measure: CentralityMeasure,
    /// Whether edge direction was followed.
    #[serde(default)]
    pub directed: bool,
    /// How many nodes the graph holds.
    #[serde(default)]
    pub nodes: i64,
    /// The top nodes, highest first.
    #[serde(default)]
    pub results: Vec<GraphScore>,
    /// Whether iteration converged. Eigenvector only.
    #[serde(default)]
    pub converged: Option<bool>,
    /// How many iterations ran. Eigenvector only.
    #[serde(default)]
    pub iterations: Option<usize>,
}

/// What [`Client::graph_communities`] should consider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommunityOptions {
    /// Consider only edges with this label.
    pub label: Option<String>,
    /// The iteration bound.
    pub iterations: Option<usize>,
    /// Cap the communities listed.
    pub limit: Option<usize>,
}

/// The communities of a graph, largest first.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphCommunities {
    /// The algorithm that found them.
    #[serde(default)]
    pub algorithm: String,
    /// How many communities there are.
    #[serde(default)]
    pub count: usize,
    /// The partition's modularity.
    #[serde(default)]
    pub modularity: f64,
    /// Whether the optimisation converged.
    #[serde(default)]
    pub converged: bool,
    /// How many passes it ran.
    #[serde(default)]
    pub iterations: usize,
    /// The communities listed.
    #[serde(default)]
    pub communities: Vec<GraphGroup>,
}

/// One node's triangles.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphNodeTriangles {
    /// The node's id.
    pub id: String,
    /// How many triangles it is a corner of.
    #[serde(default)]
    pub triangles: i64,
    /// Its local clustering coefficient.
    #[serde(default)]
    pub clustering: f64,
}

/// The answer to [`Client::graph_triangle_count`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphTriangles {
    /// How many triangles the graph has.
    #[serde(default)]
    pub triangles: i64,
    /// The graph's transitivity.
    #[serde(default)]
    pub transitivity: f64,
    /// The mean local clustering coefficient.
    #[serde(default)]
    pub average_clustering: f64,
    /// The per-node counts listed.
    #[serde(default)]
    pub nodes: Vec<GraphNodeTriangles>,
}

/// What [`Client::graph_node_similarity`] should compare.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SimilarityOptions {
    /// Which edges define a node's neighbours. The server's default is both.
    pub direction: Option<GraphDirection>,
    /// Consider only edges with this label.
    pub label: Option<String>,
    /// How to compare neighbour sets. The server's default is Jaccard.
    pub metric: Option<SimilarityMetric>,
    /// Cap the nodes returned.
    pub limit: Option<usize>,
}

/// One class of finding in a [`GraphConsistency`].
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct GraphFindings {
    /// How many were found.
    #[serde(default)]
    pub count: i64,
    /// The ones listed, as the server describes them.
    #[serde(default)]
    pub listed: Vec<Value>,
}

/// How the resident mirror compares with storage.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct GraphMirrorCheck {
    /// Whether the mirror was loaded and so could be compared.
    #[serde(default)]
    pub loaded: bool,
    /// Where mirror and storage disagree.
    #[serde(default)]
    pub differences: GraphFindings,
}

/// The answer to [`Client::graph_check_consistency`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphConsistency {
    /// Whether nothing was found.
    #[serde(default)]
    pub consistent: bool,
    /// How many nodes were checked.
    #[serde(default)]
    pub nodes: i64,
    /// How many edges were checked.
    #[serde(default)]
    pub edges: i64,
    /// Edges whose endpoint no longer exists.
    #[serde(default)]
    pub dangling_edges: GraphFindings,
    /// Records that could not be decoded.
    #[serde(default)]
    pub undecodable: GraphFindings,
    /// Records stored under another id.
    #[serde(default)]
    pub misfiled: GraphFindings,
    /// The resident mirror against storage.
    #[serde(default)]
    pub mirror: GraphMirrorCheck,
}

/// One node index.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphIndex {
    /// The index's name.
    pub name: String,
    /// The label it is limited to, when it is.
    #[serde(default)]
    pub label: Option<String>,
    /// The properties it indexes.
    #[serde(default)]
    pub properties: Vec<String>,
}

/// One constraint.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GraphConstraint {
    /// The constraint's name.
    pub name: String,
    /// The label it applies to.
    #[serde(default)]
    pub label: String,
    /// The property it rules.
    #[serde(default)]
    pub property: String,
    /// The rule.
    pub kind: ConstraintKind,
    /// Whether it applies to nodes or relationships.
    #[serde(default)]
    pub target: ConstraintTarget,
}

/// A graph's indexes and constraints.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct GraphSchemaObjects {
    /// The node indexes.
    #[serde(default)]
    pub indexes: Vec<GraphIndex>,
    /// The constraints.
    #[serde(default)]
    pub constraints: Vec<GraphConstraint>,
}

/// The answer to [`Client::graph_find_nodes`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphFoundNodes {
    /// How many nodes came back.
    #[serde(default)]
    pub count: usize,
    /// The nodes, in id order.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub nodes: Vec<GraphNode>,
    /// How the request was answered: an index by name, the label index, or a
    /// scan.
    #[serde(default)]
    pub plan: String,
    /// Whether the limit cut the result short.
    #[serde(default)]
    pub truncated: bool,
}

/// What [`Client::graph_page_rank`] should compute.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageRankOptions {
    /// The damping factor, in `[0, 1)`. The server's default is 0.85.
    pub damping: Option<f64>,
    /// The iteration bound. The server's default is 20, capped at 100.
    pub iterations: Option<usize>,
    /// The total change below which iteration stops.
    pub tolerance: Option<f64>,
    /// Follow only edges with this label.
    pub label: Option<String>,
    /// How many nodes to return. The server's default is 20, capped at 1000.
    pub limit: Option<usize>,
}

/// The answer to [`Client::graph_page_rank`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphPageRank {
    /// The top nodes by rank. The ranks of the whole graph sum to 1.
    #[serde(default)]
    pub ranks: Vec<GraphScore>,
    /// How many nodes were ranked.
    #[serde(default)]
    pub count: usize,
    /// The damping factor that was applied.
    #[serde(default)]
    pub damping: f64,
    /// How many iterations ran.
    #[serde(default)]
    pub iterations: usize,
    /// Whether iteration converged before its bound.
    #[serde(default)]
    pub converged: bool,
    /// Whether more nodes were ranked than are listed.
    #[serde(default)]
    pub truncated: bool,
}

/// What a Cypher write changed.
///
/// Inside a transaction a statement is queued rather than applied: the counts
/// are then zero and [`GraphWriteSummary::queued`] says how many statements the
/// transaction holds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphWriteSummary {
    /// How many statements ran.
    #[serde(default)]
    pub statements: i64,
    /// Nodes created.
    #[serde(default)]
    pub nodes_created: i64,
    /// Nodes deleted.
    #[serde(default)]
    pub nodes_deleted: i64,
    /// Relationships created.
    #[serde(default)]
    pub relationships_created: i64,
    /// Relationships deleted.
    #[serde(default)]
    pub relationships_deleted: i64,
    /// Properties set.
    #[serde(default)]
    pub properties_set: i64,
    /// Labels added.
    #[serde(default)]
    pub labels_added: i64,
    /// Labels removed.
    #[serde(default)]
    pub labels_removed: i64,
    /// Inside a transaction, how many statements it now holds.
    #[serde(default)]
    pub queued: Option<i64>,
    /// At a commit, how many queued statements were applied.
    #[serde(default)]
    pub committed: Option<i64>,
}

fn op(body: Value) -> Value {
    json!({"Graph": body})
}

/// Write a value only when the caller chose one, so an unset option lands on
/// the server's default instead of being sent as an empty string.
fn put(body: &mut Map<String, Value>, key: &str, value: Option<impl Into<Value>>) {
    if let Some(value) = value {
        body.insert(key.to_string(), value.into());
    }
}

fn direction_value(direction: Option<GraphDirection>) -> Option<Value> {
    direction.map(|d| serde_json::to_value(d).expect("a direction always encodes"))
}

impl Client {
    /// Create an empty graph.
    pub fn graph_create(&mut self, graph: &str) -> Result<()> {
        self.send(op(json!({"CreateGraph": {"graph": graph}})))
            .map(|_| ())
    }

    /// Drop a graph with its nodes and edges. Dropping one that does not exist
    /// is an error.
    pub fn graph_drop(&mut self, graph: &str) -> Result<()> {
        self.send(op(json!({"DropGraph": {"graph": graph}})))
            .map(|_| ())
    }

    /// Name every graph in the database.
    pub fn graph_list(&mut self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Graphs {
            #[serde(default)]
            graphs: Vec<String>,
        }
        let response = self.send(op(Value::String("ListGraphs".into())))?;
        let out: Graphs = response.decode("ListGraphs")?;
        Ok(out.graphs)
    }

    /// Store a node, replacing whatever was under that id.
    pub fn graph_add_node(
        &mut self,
        graph: &str,
        id: &str,
        labels: &[&str],
        properties: Option<&Map<String, Value>>,
    ) -> Result<()> {
        self.send(op(json!({"AddNode": {
            "graph": graph,
            "id": id,
            "labels": labels,
            "properties": properties,
        }})))
        .map(|_| ())
    }

    /// Fetch one node. `None` when there is no such id.
    pub fn graph_get_node(&mut self, graph: &str, id: &str) -> Result<Option<GraphNode>> {
        let response = self.send(op(json!({"GetNode": {"graph": graph, "id": id}})))?;
        if response.expect("Json", "GetNode")?.is_null() {
            return Ok(None);
        }
        response.decode("GetNode").map(Some)
    }

    /// Delete a node. Deleting an absent node is not an error; a missing graph
    /// is.
    pub fn graph_delete_node(&mut self, graph: &str, id: &str) -> Result<()> {
        self.send(op(json!({"DeleteNode": {"graph": graph, "id": id}})))
            .map(|_| ())
    }

    /// One page of a graph's nodes, ordered by id.
    pub fn graph_list_nodes(
        &mut self,
        graph: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<GraphNodePage> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "limit", limit.filter(|l| *l > 0));
        put(&mut body, "offset", offset.filter(|o| *o > 0));
        let response = self.send(op(json!({"ListNodes": Value::Object(body)})))?;
        response.decode("ListNodes")
    }

    /// Store a directed edge. Both endpoints must already exist: a dangling
    /// endpoint is refused rather than created.
    pub fn graph_add_edge(
        &mut self,
        graph: &str,
        id: &str,
        from: &str,
        to: &str,
        label: &str,
        properties: Option<&Map<String, Value>>,
    ) -> Result<()> {
        self.send(op(json!({"AddEdge": {
            "graph": graph,
            "id": id,
            "from": from,
            "to": to,
            "label": label,
            "properties": properties,
        }})))
        .map(|_| ())
    }

    /// Fetch one edge. `None` when there is no such id.
    pub fn graph_get_edge(&mut self, graph: &str, id: &str) -> Result<Option<GraphEdge>> {
        let response = self.send(op(json!({"GetEdge": {"graph": graph, "id": id}})))?;
        if response.expect("Json", "GetEdge")?.is_null() {
            return Ok(None);
        }
        response.decode("GetEdge").map(Some)
    }

    /// Delete an edge. Deleting an absent edge is not an error; a missing graph
    /// is.
    pub fn graph_delete_edge(&mut self, graph: &str, id: &str) -> Result<()> {
        self.send(op(json!({"DeleteEdge": {"graph": graph, "id": id}})))
            .map(|_| ())
    }

    /// One page of a graph's edges, ordered by id.
    pub fn graph_list_edges(
        &mut self,
        graph: &str,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<GraphEdgePage> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "limit", limit.filter(|l| *l > 0));
        put(&mut body, "offset", offset.filter(|o| *o > 0));
        let response = self.send(op(json!({"ListEdges": Value::Object(body)})))?;
        response.decode("ListEdges")
    }

    /// The edges incident to a node, and the node at the far end of each. A
    /// node with no matching edges gives an empty list, not an error.
    pub fn graph_neighbors(
        &mut self,
        graph: &str,
        node_id: &str,
        options: &NeighborOptions,
    ) -> Result<Vec<GraphNeighbor>> {
        #[derive(Deserialize)]
        struct Neighbors {
            #[serde(default)]
            neighbors: Vec<GraphNeighbor>,
        }
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("node_id".into(), json!(node_id));
        put(&mut body, "direction", direction_value(options.direction));
        put(&mut body, "label", options.label.clone());
        put(&mut body, "limit", options.limit.filter(|l| *l > 0));
        let response = self.send(op(json!({"Neighbors": Value::Object(body)})))?;
        let out: Neighbors = response.decode("Neighbors")?;
        Ok(out.neighbors)
    }

    /// How many edges are incident to a node in a direction. `Both` counts each
    /// edge once, self-loops included.
    pub fn graph_degree(
        &mut self,
        graph: &str,
        node_id: &str,
        direction: Option<GraphDirection>,
    ) -> Result<i64> {
        #[derive(Deserialize)]
        struct Degree {
            #[serde(default)]
            degree: i64,
        }
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("node_id".into(), json!(node_id));
        put(&mut body, "direction", direction_value(direction));
        let response = self.send(op(json!({"Degree": Value::Object(body)})))?;
        let out: Degree = response.decode("Degree")?;
        Ok(out.degree)
    }

    /// Walk outward from a node by breadth-first search. The start node must
    /// exist.
    pub fn graph_traverse(
        &mut self,
        graph: &str,
        start: &str,
        options: &TraverseOptions,
    ) -> Result<GraphTraversal> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("start".into(), json!(start));
        put(&mut body, "direction", direction_value(options.direction));
        put(&mut body, "label", options.label.clone());
        put(&mut body, "max_depth", options.max_depth.filter(|d| *d > 0));
        put(&mut body, "limit", options.limit.filter(|l| *l > 0));
        let response = self.send(op(json!({"Traverse": Value::Object(body)})))?;
        response.decode("Traverse")
    }

    /// The path with the fewest hops between two nodes. Both must exist. "No
    /// path" comes back as `found == false`, not as an error.
    pub fn graph_shortest_path(
        &mut self,
        graph: &str,
        from: &str,
        to: &str,
        options: &PathOptions,
    ) -> Result<GraphPath> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("from".into(), json!(from));
        body.insert("to".into(), json!(to));
        put(&mut body, "direction", direction_value(options.direction));
        put(&mut body, "label", options.label.clone());
        put(&mut body, "max_depth", options.max_depth.filter(|d| *d > 0));
        let response = self.send(op(json!({"ShortestPath": Value::Object(body)})))?;
        response.decode("ShortestPath")
    }

    /// The least-cost path between two nodes, by summed edge weight.
    ///
    /// A different question from [`Client::graph_shortest_path`], which
    /// minimises hops: with unequal weights the two return different paths and
    /// neither substitutes for the other.
    pub fn graph_weighted_shortest_path(
        &mut self,
        graph: &str,
        from: &str,
        to: &str,
        options: &WeightedPathOptions,
    ) -> Result<GraphPath> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("from".into(), json!(from));
        body.insert("to".into(), json!(to));
        put(&mut body, "direction", direction_value(options.direction));
        put(&mut body, "label", options.label.clone());
        put(
            &mut body,
            "weight_property",
            options.weight_property.clone(),
        );
        let response = self.send(op(json!({"WeightedShortestPath": Value::Object(body)})))?;
        response.decode("WeightedShortestPath")
    }

    /// Run a read-only Cypher query.
    ///
    /// The server implements `MATCH` / `WHERE` / `RETURN` with labels, property
    /// predicates, relationship direction and type, bounded variable-length
    /// paths, `DISTINCT`, `ORDER BY` / `SKIP` / `LIMIT`, and global aggregates.
    /// Every other clause — every write clause, `OPTIONAL MATCH`, `WITH`,
    /// `UNWIND`, `CALL`, path variables, `shortestPath()`, parameters — is
    /// refused by name rather than ignored, so a rejected query is an error
    /// instead of an answer computed from half a statement.
    pub fn graph_query(&mut self, graph: &str, cypher: &str) -> Result<GraphRows> {
        let response = self.send(op(json!({"Query": {"graph": graph, "cypher": cypher}})))?;
        response.decode("Query")
    }

    /// Write many nodes and edges as one unit: every record lands or none
    /// does.
    ///
    /// Nodes are written before edges, so an edge may join nodes created in
    /// the same batch. The whole batch is validated before anything is
    /// written.
    pub fn graph_add_many(
        &mut self,
        graph: &str,
        nodes: &[GraphNodeInput],
        edges: &[GraphEdgeInput],
    ) -> Result<GraphCounts> {
        let response = self.send(op(json!({"AddMany": {
            "graph": graph,
            "nodes": nodes,
            "edges": edges,
        }})))?;
        response.decode("AddMany")
    }

    /// Delete many nodes and edges as one unit. An id that is not there is
    /// skipped. With `detach`, every edge touching a removed node goes too.
    pub fn graph_remove_many(
        &mut self,
        graph: &str,
        nodes: &[&str],
        edges: &[&str],
        detach: bool,
    ) -> Result<GraphRemoved> {
        let response = self.send(op(json!({"RemoveMany": {
            "graph": graph,
            "nodes": nodes,
            "edges": edges,
            "detach": detach,
        }})))?;
        response.decode("RemoveMany")
    }

    /// Change an existing node in place, and return it as it now stands.
    pub fn graph_update_node(
        &mut self,
        graph: &str,
        id: &str,
        update: &NodeUpdate,
    ) -> Result<GraphNode> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("id".into(), json!(id));
        put(&mut body, "properties", update.properties.clone());
        if !update.add_labels.is_empty() {
            body.insert("add_labels".into(), json!(update.add_labels));
        }
        if !update.remove_labels.is_empty() {
            body.insert("remove_labels".into(), json!(update.remove_labels));
        }
        let response = self.send(op(json!({"UpdateNode": Value::Object(body)})))?;
        response.decode("UpdateNode")
    }

    /// Change an existing edge in place, and return it as it now stands.
    /// `properties` is a JSON merge patch and `label` changes the edge's type;
    /// the endpoints cannot change.
    pub fn graph_update_edge(
        &mut self,
        graph: &str,
        id: &str,
        properties: Option<&Value>,
        label: Option<&str>,
    ) -> Result<GraphEdge> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("id".into(), json!(id));
        put(&mut body, "properties", properties.cloned());
        put(&mut body, "label", label);
        let response = self.send(op(json!({"UpdateEdge": Value::Object(body)})))?;
        response.decode("UpdateEdge")
    }

    /// Copy a graph — nodes, edges and schema — to a new name. Refused when
    /// `to` exists.
    pub fn graph_copy(&mut self, graph: &str, to: &str) -> Result<GraphCounts> {
        let response = self.send(op(json!({"CopyGraph": {"graph": graph, "to": to}})))?;
        response.decode("CopyGraph")
    }

    /// Walk outward from a node depth-first. Returns exactly the nodes
    /// [`Client::graph_traverse`] does, in DFS preorder.
    pub fn graph_dfs(
        &mut self,
        graph: &str,
        start: &str,
        options: &TraverseOptions,
    ) -> Result<GraphTraversal> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("start".into(), json!(start));
        put_walk(&mut body, options);
        let response = self.send(op(json!({"Dfs": Value::Object(body)})))?;
        response.decode("Dfs")
    }

    /// Partition the graph's nodes into connected components, largest first.
    pub fn graph_connected_components(
        &mut self,
        graph: &str,
        options: &ComponentOptions,
    ) -> Result<GraphComponents> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "kind", options.kind.map(|k| json!(k)));
        put(&mut body, "label", options.label.clone());
        put(&mut body, "limit", options.limit);
        let response = self.send(op(json!({"ConnectedComponents": Value::Object(body)})))?;
        response.decode("ConnectedComponents")
    }

    /// Whether the graph contains a cycle, with one cycle as evidence.
    ///
    /// `directed` of `None` leaves it to the server, which follows edge
    /// direction. Undirected, two parallel edges between a pair are a cycle
    /// and a single edge is not.
    pub fn graph_detect_cycle(
        &mut self,
        graph: &str,
        directed: Option<bool>,
        label: Option<&str>,
    ) -> Result<GraphCycleCheck> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "directed", directed);
        put(&mut body, "label", label);
        let response = self.send(op(json!({"DetectCycle": Value::Object(body)})))?;
        response.decode("DetectCycle")
    }

    /// Counts describing a graph.
    pub fn graph_stats(&mut self, graph: &str) -> Result<GraphStats> {
        let response = self.send(op(json!({"GraphStats": {"graph": graph}})))?;
        response.decode("GraphStats")
    }

    /// The part of the graph within some hops of `seeds`: the nodes reached
    /// and every edge joining two of them.
    ///
    /// The server's defaults differ from a traversal's: both directions, one
    /// hop, and up to 1000 nodes.
    pub fn graph_subgraph(
        &mut self,
        graph: &str,
        seeds: &[&str],
        options: &TraverseOptions,
    ) -> Result<GraphSubgraph> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("seeds".into(), json!(seeds));
        put_walk(&mut body, options);
        let response = self.send(op(json!({"Subgraph": Value::Object(body)})))?;
        response.decode("Subgraph")
    }

    /// What the graph's data looks like: its node labels and edge types, each
    /// with a count and the property keys seen on it.
    pub fn graph_schema(&mut self, graph: &str) -> Result<GraphSchema> {
        let response = self.send(op(json!({"GraphSchema": {"graph": graph}})))?;
        response.decode("GraphSchema")
    }

    /// The `k` cheapest loopless paths between two nodes, cheapest first.
    pub fn graph_k_shortest_paths(
        &mut self,
        graph: &str,
        from: &str,
        to: &str,
        options: &KPathOptions,
    ) -> Result<GraphPaths> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("from".into(), json!(from));
        body.insert("to".into(), json!(to));
        put(&mut body, "k", options.k.filter(|k| *k > 0));
        put(&mut body, "direction", direction_value(options.direction));
        put(&mut body, "label", options.label.clone());
        put(
            &mut body,
            "weight_property",
            options.weight_property.clone(),
        );
        let response = self.send(op(json!({"KShortestPaths": Value::Object(body)})))?;
        response.decode("KShortestPaths")
    }

    /// Every simple path between two nodes within a hop bound. A work budget
    /// bounds the search, and [`GraphPaths::complete`] says whether it
    /// finished.
    pub fn graph_all_paths(
        &mut self,
        graph: &str,
        from: &str,
        to: &str,
        options: &TraverseOptions,
    ) -> Result<GraphPaths> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("from".into(), json!(from));
        body.insert("to".into(), json!(to));
        put_walk(&mut body, options);
        let response = self.send(op(json!({"AllPaths": Value::Object(body)})))?;
        response.decode("AllPaths")
    }

    /// Betweenness, closeness or eigenvector centrality, top nodes first.
    pub fn graph_centrality(
        &mut self,
        graph: &str,
        measure: CentralityMeasure,
        options: &CentralityOptions,
    ) -> Result<GraphCentrality> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("measure".into(), json!(measure));
        put(&mut body, "directed", options.directed);
        put(&mut body, "label", options.label.clone());
        put(&mut body, "limit", options.limit);
        put(&mut body, "iterations", options.iterations);
        put(&mut body, "tolerance", options.tolerance);
        let response = self.send(op(json!({"Centrality": Value::Object(body)})))?;
        response.decode("Centrality")
    }

    /// Communities by Louvain modularity optimisation, direction ignored.
    pub fn graph_communities(
        &mut self,
        graph: &str,
        options: &CommunityOptions,
    ) -> Result<GraphCommunities> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "label", options.label.clone());
        put(&mut body, "iterations", options.iterations);
        put(&mut body, "limit", options.limit);
        let response = self.send(op(json!({"Communities": Value::Object(body)})))?;
        response.decode("Communities")
    }

    /// Triangles and clustering coefficients, direction ignored.
    pub fn graph_triangle_count(
        &mut self,
        graph: &str,
        label: Option<&str>,
        limit: Option<usize>,
    ) -> Result<GraphTriangles> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "label", label);
        put(&mut body, "limit", limit);
        let response = self.send(op(json!({"TriangleCount": Value::Object(body)})))?;
        response.decode("TriangleCount")
    }

    /// The nodes whose neighbour sets most resemble `node`'s, most similar
    /// first.
    pub fn graph_node_similarity(
        &mut self,
        graph: &str,
        node: &str,
        options: &SimilarityOptions,
    ) -> Result<Vec<GraphScore>> {
        #[derive(Deserialize)]
        struct Similar {
            #[serde(default)]
            results: Vec<GraphScore>,
        }
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("node".into(), json!(node));
        put(&mut body, "direction", direction_value(options.direction));
        put(&mut body, "label", options.label.clone());
        put(&mut body, "metric", options.metric.map(|m| json!(m)));
        put(&mut body, "limit", options.limit);
        let response = self.send(op(json!({"NodeSimilarity": Value::Object(body)})))?;
        let out: Similar = response.decode("NodeSimilarity")?;
        Ok(out.results)
    }

    /// Check a graph's stored records against each other and against the
    /// resident mirror.
    pub fn graph_check_consistency(&mut self, graph: &str) -> Result<GraphConsistency> {
        let response = self.send(op(json!({"CheckConsistency": {"graph": graph}})))?;
        response.decode("CheckConsistency")
    }

    /// Index node properties for [`Client::graph_find_nodes`]. Several
    /// properties make a composite index that serves an equality on all of
    /// them; `label` limits it to nodes carrying that label.
    pub fn graph_create_node_index(
        &mut self,
        graph: &str,
        name: &str,
        label: Option<&str>,
        properties: &[&str],
    ) -> Result<()> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        body.insert("name".into(), json!(name));
        put(&mut body, "label", label);
        body.insert("properties".into(), json!(properties));
        self.send(op(json!({"CreateNodeIndex": Value::Object(body)})))
            .map(|_| ())
    }

    /// Enforce a rule on a property of every node — or, with
    /// [`ConstraintTarget::Edge`], every relationship — labelled `label`.
    /// Refused when a stored record already breaks it.
    pub fn graph_create_constraint(
        &mut self,
        graph: &str,
        name: &str,
        label: &str,
        property: &str,
        kind: ConstraintKind,
        target: ConstraintTarget,
    ) -> Result<()> {
        self.send(op(json!({"CreateConstraint": {
            "graph": graph,
            "name": name,
            "label": label,
            "property": property,
            "kind": kind,
            "target": target,
        }})))
        .map(|_| ())
    }

    /// Drop an index or a constraint by name.
    pub fn graph_drop_schema_object(&mut self, graph: &str, name: &str) -> Result<()> {
        self.send(op(
            json!({"DropSchemaObject": {"graph": graph, "name": name}}),
        ))
        .map(|_| ())
    }

    /// A graph's indexes and constraints.
    pub fn graph_list_schema_objects(&mut self, graph: &str) -> Result<GraphSchemaObjects> {
        let response = self.send(op(json!({"ListSchemaObjects": {"graph": graph}})))?;
        response.decode("ListSchemaObjects")
    }

    /// Nodes with `label`, when given, whose properties equal these values, in
    /// id order. [`GraphFoundNodes::plan`] says whether an index answered.
    pub fn graph_find_nodes(
        &mut self,
        graph: &str,
        label: Option<&str>,
        properties: Option<&Map<String, Value>>,
        limit: Option<usize>,
    ) -> Result<GraphFoundNodes> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "label", label);
        put(&mut body, "properties", properties.cloned());
        put(&mut body, "limit", limit);
        let response = self.send(op(json!({"FindNodes": Value::Object(body)})))?;
        response.decode("FindNodes")
    }

    /// Import nodes and edges from CSV text as one atomic batch, through the
    /// same path as [`Client::graph_add_many`], so constraints apply.
    pub fn graph_import_csv(
        &mut self,
        graph: &str,
        nodes: Option<&str>,
        edges: Option<&str>,
    ) -> Result<GraphCounts> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "nodes", nodes);
        put(&mut body, "edges", edges);
        let response = self.send(op(json!({"ImportCsv": Value::Object(body)})))?;
        response.decode("ImportCsv")
    }

    /// PageRank over edge direction, top nodes first.
    pub fn graph_page_rank(
        &mut self,
        graph: &str,
        options: &PageRankOptions,
    ) -> Result<GraphPageRank> {
        let mut body = Map::new();
        body.insert("graph".into(), json!(graph));
        put(&mut body, "damping", options.damping);
        put(&mut body, "iterations", options.iterations);
        put(&mut body, "tolerance", options.tolerance);
        put(&mut body, "label", options.label.clone());
        put(&mut body, "limit", options.limit);
        let response = self.send(op(json!({"PageRank": Value::Object(body)})))?;
        response.decode("PageRank")
    }

    /// Run a Cypher write — `CREATE`, `MERGE`, `SET`, `REMOVE`, `[DETACH]
    /// DELETE`, optionally after `MATCH … [WHERE …]`.
    ///
    /// Separate from [`Client::graph_query`] because it writes. While
    /// [`Client::set_request_txn`] names a graph transaction the statement is
    /// queued rather than applied.
    pub fn graph_execute(&mut self, graph: &str, cypher: &str) -> Result<GraphWriteSummary> {
        let response = self.send(op(json!({"Execute": {"graph": graph, "cypher": cypher}})))?;
        response.decode("Execute")
    }

    /// Open a transaction on a graph, and return its id.
    ///
    /// Pass the id to [`Client::set_request_txn`]: every
    /// [`Client::graph_execute`] sent while it is set is queued, and the
    /// commit applies them all or none.
    pub fn graph_begin_transaction(&mut self, graph: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Begun {
            #[serde(default)]
            transaction: String,
        }
        let response = self.send_in(op(json!({"BeginTransaction": {"graph": graph}})), None)?;
        let out: Begun = response.decode("BeginTransaction")?;
        if out.transaction.is_empty() {
            return Err(Error::protocol(
                "BeginTransaction answered with no transaction id",
            ));
        }
        Ok(out.transaction)
    }

    /// Apply every statement queued in a transaction as one write: all of
    /// them or none. Clears [`Client::set_request_txn`] when it names this
    /// transaction.
    pub fn graph_commit_transaction(
        &mut self,
        graph: &str,
        txn: &str,
    ) -> Result<GraphWriteSummary> {
        let outcome = self.send_in(
            op(json!({"CommitTransaction": {"graph": graph}})),
            Some(txn),
        );
        self.forget_txn(txn);
        outcome?.decode("CommitTransaction")
    }

    /// Discard a transaction and its queued statements, returning how many
    /// there were.
    pub fn graph_rollback_transaction(&mut self, graph: &str, txn: &str) -> Result<i64> {
        #[derive(Deserialize)]
        struct RolledBack {
            #[serde(default)]
            rolled_back: i64,
        }
        let outcome = self.send_in(
            op(json!({"RollbackTransaction": {"graph": graph}})),
            Some(txn),
        );
        self.forget_txn(txn);
        let out: RolledBack = outcome?.decode("RollbackTransaction")?;
        Ok(out.rolled_back)
    }
}

/// The four bounds a walk takes, each only when the caller chose one.
fn put_walk(body: &mut Map<String, Value>, options: &TraverseOptions) {
    put(body, "direction", direction_value(options.direction));
    put(body, "label", options.label.clone());
    put(body, "max_depth", options.max_depth.filter(|d| *d > 0));
    put(body, "limit", options.limit.filter(|l| *l > 0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directions_use_the_servers_spelling() {
        assert_eq!(
            serde_json::to_value(GraphDirection::Outgoing).unwrap(),
            "outgoing"
        );
        assert_eq!(
            serde_json::to_value(GraphDirection::Incoming).unwrap(),
            "incoming"
        );
        assert_eq!(serde_json::to_value(GraphDirection::Both).unwrap(), "both");
        assert_eq!(GraphDirection::default(), GraphDirection::Outgoing);
    }

    #[test]
    fn an_unset_option_is_left_out_rather_than_sent_empty() {
        let mut body = Map::new();
        put(&mut body, "label", None::<String>);
        put(&mut body, "limit", None::<usize>);
        assert!(body.is_empty());

        put(&mut body, "label", Some("FOLLOWS".to_string()));
        put(&mut body, "limit", Some(10usize));
        assert_eq!(body["label"], "FOLLOWS");
        assert_eq!(body["limit"], 10);
    }

    #[test]
    fn a_zero_bound_is_the_servers_default_not_a_bound_of_zero() {
        let options = TraverseOptions {
            max_depth: Some(0),
            ..Default::default()
        };
        let mut body = Map::new();
        put(&mut body, "max_depth", options.max_depth.filter(|d| *d > 0));
        assert!(body.is_empty());
    }

    #[test]
    fn a_path_that_was_not_found_still_decodes() {
        let path: GraphPath =
            serde_json::from_value(json!({"found": false, "message": "no path within 10 hops"}))
                .unwrap();
        assert!(!path.found);
        assert_eq!(path.hops, 0);
        assert!(path.node_path.is_empty());
    }
}
