import { useEffect, useRef } from "react";
import Sigma from "sigma";
import forceAtlas2 from "graphology-layout-forceatlas2";
import { useGraphData } from "../../hooks/useGraphData";
import type { QueryResponse } from "../../types/api";

interface GraphViewProps {
  result: QueryResponse;
  onNodeSelect?: (nodeId: string | null) => void;
  selectedNodeId?: string | null;
}

export default function GraphView({ result, onNodeSelect, selectedNodeId }: GraphViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const sigmaRef = useRef<Sigma | null>(null);
  const graphData = useGraphData(result);
  const graph = graphData?.graph ?? null;

  useEffect(() => {
    if (sigmaRef.current) {
      sigmaRef.current.kill();
      sigmaRef.current = null;
    }

    if (!containerRef.current || !graph || graph.order === 0) return;

    // Apply ForceAtlas2 layout (synchronous, a few iterations)
    forceAtlas2.assign(graph, {
      iterations: 100,
      settings: {
        gravity: 1,
        scalingRatio: 10,
        barnesHutOptimize: graph.order > 100,
      },
    });

    const renderer = new Sigma(graph, containerRef.current, {
      renderEdgeLabels: true,
      defaultEdgeType: "arrow",
      labelColor: { color: "#e2e8f0" },
      labelSize: 12,
      labelFont: "Inter, sans-serif",
    });

    renderer.on("clickNode", ({ node }) => {
      onNodeSelect?.(node);
    });

    renderer.on("clickStage", () => {
      onNodeSelect?.(null);
    });

    sigmaRef.current = renderer;

    return () => {
      renderer.kill();
      sigmaRef.current = null;
    };
  }, [graph, onNodeSelect]);

  // Highlight selected node
  useEffect(() => {
    if (!graph || !sigmaRef.current) return;
    graph.forEachNode((nodeId) => {
      const attrs = graph.getNodeAttributes(nodeId);
      if (!attrs._originalSize) {
        graph.setNodeAttribute(nodeId, "_originalSize", attrs.size);
      }
      graph.setNodeAttribute(
        nodeId,
        "size",
        nodeId === selectedNodeId ? (attrs._originalSize as number ?? 10) * 1.5 : (attrs._originalSize as number ?? 10),
      );
    });
    sigmaRef.current.refresh();
  }, [selectedNodeId, graph]);

  if (!graph || graph.order === 0) {
    return (
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          height: "100%",
          color: "var(--text-muted)",
          fontSize: "15px",
          flexDirection: "column",
          gap: "8px",
        }}
      >
        <span>No graph data to display</span>
        <span style={{ fontSize: "13px" }}>
          Try a query that returns nodes and edges, e.g.{" "}
          <code style={{ color: "var(--accent)" }}>
            MATCH (a)-[r]-&gt;(b) RETURN a, r, b
          </code>
        </span>
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      style={{
        width: "100%",
        height: "100%",
        background: "var(--bg-primary)",
      }}
    />
  );
}
