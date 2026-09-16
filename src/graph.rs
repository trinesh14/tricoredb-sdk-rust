//! Graphs: labelled nodes, typed edges, traversal, paths and Cypher.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::client::Client;
use crate::error::Result;

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
