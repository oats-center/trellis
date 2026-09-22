<script lang="ts">
  import { onMount } from "svelte";
  import type { Attachment } from "svelte/attachments";

  export type DeploymentUsesGraphNode = {
    id: string;
    label: string;
    kind: "deployment" | "contract" | "external-contract";
    detail?: string;
    status?: "healthy" | "degraded" | "unhealthy" | "offline";
    weight?: number;
  };

  export type DeploymentUsesGraphEdge = {
    id?: string;
    source: string;
    target: string;
    label?: string;
    kind?: "applies" | "uses";
    weight?: number;
  };

  type Props = {
    nodes: DeploymentUsesGraphNode[];
    edges: DeploymentUsesGraphEdge[];
    title?: string;
    description?: string;
    class?: string;
  };

  type GraphAttributes = Record<string, string | number | boolean>;
  type MutableGraph = {
    addNode(id: string, attributes?: GraphAttributes): void;
    addDirectedEdgeWithKey(key: string, source: string, target: string, attributes?: GraphAttributes): void;
  };
  type GraphConstructor = new () => MutableGraph;
  type SigmaSettings = Record<string, unknown>;
  type SigmaRenderer = { kill(): void };
  type SigmaConstructor = new (graph: MutableGraph, container: HTMLElement, settings?: SigmaSettings) => SigmaRenderer;
  type ForceAtlasOptions = { iterations: number; settings: Record<string, number | boolean> };
  type ForceAtlasModule = { assign(graph: MutableGraph, options: ForceAtlasOptions): void };
  type GraphRuntime = {
    Graph: GraphConstructor;
    Sigma: SigmaConstructor;
    forceAtlas2: ForceAtlasModule;
  };

  let {
    nodes,
    edges,
    title = "Deployment uses graph",
    description = "Runtime view of service deployments, applied contracts, and declared cross-contract uses.",
    class: className = "",
  }: Props = $props();

  const componentId = $props.id();
  const nodeIds = $derived(new Set(nodes.map((node) => node.id)));
  const visibleEdges = $derived(edges.filter((edge) => nodeIds.has(edge.source) && nodeIds.has(edge.target)));
  const statusCounts = $derived({
    degraded: nodes.filter((node) => node.status === "degraded").length,
    unhealthy: nodes.filter((node) => node.status === "unhealthy").length,
  });
  const graphSummary = $derived(
    `${nodes.length} node${nodes.length === 1 ? "" : "s"} and ${visibleEdges.length} edge${visibleEdges.length === 1 ? "" : "s"}.`,
  );

  let container = $state<HTMLDivElement>();
  let runtime = $state<GraphRuntime | null>(null);
  let renderer: SigmaRenderer | null = null;
  let loading = $state(false);
  let error = $state<string | null>(null);
  let renderedSignature = "";
  let destroyed = false;

  function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === "object" && value !== null;
  }

  function readExport(module: unknown, exportName: "default" | "Graph" | "Sigma"): unknown {
    return isRecord(module) ? module[exportName] : undefined;
  }

  function isConstructor(value: unknown): value is GraphConstructor & SigmaConstructor {
    return typeof value === "function";
  }

  function isForceAtlasModule(value: unknown): value is ForceAtlasModule {
    return isRecord(value) && typeof value.assign === "function";
  }

  function readForceAtlas(module: unknown): ForceAtlasModule | null {
    const candidate = isRecord(module) && isRecord(module.default) ? module.default : module;
    return isForceAtlasModule(candidate) ? candidate : null;
  }

  async function loadRuntime(): Promise<GraphRuntime> {
    const [sigmaModule, graphologyModule, forceAtlasModule]: [unknown, unknown, unknown] = await Promise.all([
      import("sigma"),
      import("graphology"),
      import("graphology-layout-forceatlas2"),
    ]);

    const Sigma = readExport(sigmaModule, "default") ?? readExport(sigmaModule, "Sigma");
    const Graph = readExport(graphologyModule, "default") ?? readExport(graphologyModule, "Graph");
    const forceAtlas2 = readForceAtlas(forceAtlasModule);
    if (!isConstructor(Sigma) || !isConstructor(Graph) || !forceAtlas2) {
      throw new Error("Graph renderer modules did not expose the expected runtime API.");
    }

    return { Graph, Sigma, forceAtlas2 };
  }

  function token(name: string, fallback: string): string {
    if (!container) return fallback;
    const value = getComputedStyle(container).getPropertyValue(name).trim();
    return value || fallback;
  }

  function colorForNode(node: DeploymentUsesGraphNode): string {
    if (node.status === "unhealthy") return token("--color-error", "#ef4444");
    if (node.status === "degraded") return token("--color-warning", "#f59e0b");
    if (node.status === "healthy") return token("--color-success", "#10b981");
    if (node.kind === "deployment") return token("--color-primary", "#10b981");
    if (node.kind === "contract") return token("--color-secondary", "#8b5cf6");
    return token("--color-warning", "#f59e0b");
  }

  function colorForEdge(edge: DeploymentUsesGraphEdge): string {
    if (edge.kind === "uses") return token("--color-accent", "#14b8a6");
    return token("--color-base-content", "#111827");
  }

  function destroyRenderer(): void {
    renderer?.kill();
    renderer = null;
  }

  function graphSignature(): string {
    return JSON.stringify({ nodes, edges: visibleEdges });
  }

  function drawIfChanged(nextRuntime: GraphRuntime, force = false): void {
    const nextSignature = graphSignature();
    if (!force && nextSignature === renderedSignature) return;
    renderedSignature = nextSignature;
    drawGraph(nextRuntime);
  }

  const graphContainer: Attachment<HTMLDivElement> = (node) => {
    container = node;
    if (runtime) drawIfChanged(runtime, true);

    return () => {
      if (container === node) container = undefined;
      destroyRenderer();
    };
  };

  function drawGraph(nextRuntime: GraphRuntime): void {
    if (!container || nodes.length === 0) return;
    destroyRenderer();

    const graph = new nextRuntime.Graph();
    const radius = Math.max(1, nodes.length) * 7;
    nodes.forEach((node, index) => {
      const angle = (index / Math.max(1, nodes.length)) * Math.PI * 2;
      graph.addNode(node.id, {
        label: node.label,
        x: Math.cos(angle) * radius,
        y: Math.sin(angle) * radius,
        size: Math.max(6, Math.min(18, node.weight ?? 8)),
        color: colorForNode(node),
        highlighted: node.kind === "deployment",
      });
    });

    visibleEdges.forEach((edge, index) => {
      graph.addDirectedEdgeWithKey(edge.id ?? `${edge.source}->${edge.target}:${index}`, edge.source, edge.target, {
        label: edge.label ?? edge.kind ?? "uses",
        size: Math.max(1, Math.min(4, edge.weight ?? 1.5)),
        color: colorForEdge(edge),
        type: "arrow",
      });
    });

    if (nodes.length > 1) {
      nextRuntime.forceAtlas2.assign(graph, {
        iterations: Math.min(140, Math.max(40, nodes.length * 8)),
        settings: {
          adjustSizes: true,
          barnesHutOptimize: nodes.length > 30,
          gravity: 0.35,
          outboundAttractionDistribution: true,
          scalingRatio: 8,
          slowDown: 8,
        },
      });
    }

    renderer = new nextRuntime.Sigma(graph, container, {
      defaultEdgeType: "arrow",
      labelColor: { color: token("--color-base-content", "#111827") },
      labelDensity: 0.08,
      labelRenderedSizeThreshold: 7,
      renderEdgeLabels: false,
      zIndex: true,
    });
  }

  onMount(() => {
    void (async () => {
      loading = true;
      error = null;
      try {
        const nextRuntime = await loadRuntime();
        if (destroyed) return;
        runtime = nextRuntime;
        drawIfChanged(nextRuntime, true);
      } catch (e) {
        error = e instanceof Error ? e.message : "Graph renderer failed to load.";
      } finally {
        loading = false;
      }
    })();
    const redrawTimer = window.setInterval(() => {
      if (runtime) drawIfChanged(runtime);
    }, 250);

    return () => {
      destroyed = true;
      window.clearInterval(redrawTimer);
      destroyRenderer();
    };
  });
</script>

<section class={["trellis-section bg-base-100", className]} aria-labelledby={`${componentId}-title`} aria-describedby={`${componentId}-description ${componentId}-fallback`}>
  <div class="border-b border-base-300 px-4 py-3">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div class="min-w-0">
        <p class="text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-base-content/45">Uses topology</p>
        <h2 id={`${componentId}-title`} class="truncate text-base font-bold leading-tight">{title}</h2>
        <p id={`${componentId}-description`} class="mt-1 text-xs text-base-content/60">{description}</p>
      </div>
      <div class="flex shrink-0 flex-wrap items-center justify-end gap-1.5 text-xs">
        <span class="badge badge-outline badge-sm">{nodes.length} nodes</span>
        <span class="badge badge-outline badge-sm">{visibleEdges.length} edges</span>
        {#if statusCounts.unhealthy > 0}<span class="badge badge-error badge-sm">{statusCounts.unhealthy} unhealthy</span>{/if}
        {#if statusCounts.degraded > 0}<span class="badge badge-warning badge-sm">{statusCounts.degraded} degraded</span>{/if}
      </div>
    </div>
  </div>

  <div class="trellis-section-body gap-3 p-4">
    <div class="flex flex-wrap gap-2 text-xs text-base-content/65" aria-hidden="true">
      <span class="inline-flex items-center gap-1"><span class="h-2 w-2 rounded-full bg-primary"></span>Deployment</span>
      <span class="inline-flex items-center gap-1"><span class="h-2 w-2 rounded-full bg-secondary"></span>Contract</span>
      <span class="inline-flex items-center gap-1"><span class="h-2 w-2 rounded-full bg-warning"></span>Unresolved contract</span>
      <span class="inline-flex items-center gap-1"><span class="h-2 w-5 rounded-full bg-accent"></span>Uses edge</span>
    </div>

    {#if nodes.length === 0}
      <div class="rounded-box border border-dashed border-base-300 bg-base-200/50 p-6 text-center">
        <p class="font-medium">No uses graph data</p>
        <p class="mt-1 text-sm text-base-content/60">Apply contracts with declared uses to render the topology.</p>
      </div>
    {:else}
      <div class="relative min-h-72 overflow-hidden rounded-box border border-base-300 bg-base-200/40 md:min-h-96">
        <div {@attach graphContainer} class="absolute inset-0" aria-hidden="true"></div>
        {#if loading}
          <div class="absolute inset-0 grid place-items-center bg-base-100/70 text-sm text-base-content/65">
            <span><span class="loading loading-spinner loading-sm mr-2"></span>Loading graph renderer</span>
          </div>
        {/if}
        {#if error}
          <div class="absolute inset-x-3 bottom-3 rounded-box border border-warning/35 bg-base-100 p-3 text-sm text-base-content">
            <span class="font-medium text-warning">Graph unavailable.</span> {error}
          </div>
        {/if}
      </div>
    {/if}

    <div id={`${componentId}-fallback`} class="sr-only">
      <p>{graphSummary}</p>
      <ul>
        {#each nodes as node (node.id)}
          <li>{node.kind}: {node.label}{node.detail ? `, ${node.detail}` : ""}{node.status ? `, ${node.status}` : ""}</li>
        {/each}
      </ul>
      <ul>
        {#each visibleEdges as edge, index (edge.id ?? `${edge.source}:${edge.target}:${index}`)}
          <li>{edge.source} {edge.label ?? edge.kind ?? "uses"} {edge.target}</li>
        {/each}
      </ul>
    </div>
  </div>
</section>
