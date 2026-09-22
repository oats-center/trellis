export type WorkflowStepId = "select" | "reconcile" | "evidence" | "closeout";

export type WorkflowStep = {
  id: WorkflowStepId;
  label: string;
  eyebrow: string;
  path: "/inspection" | "/evidence" | "/closeout";
  hash?: string;
};

export type WorkflowContext = {
  inspectionId: string | null;
  siteId: string | null;
};

export const workflowSteps: WorkflowStep[] = [
  {
    id: "select",
    label: "Select inspection",
    eyebrow: "Assignment queue",
    path: "/inspection",
  },
  {
    id: "reconcile",
    label: "Reconcile site",
    eyebrow: "Live site context",
    path: "/inspection",
  },
  {
    id: "evidence",
    label: "Verify evidence",
    eyebrow: "Chain of custody",
    path: "/evidence",
  },
  {
    id: "closeout",
    label: "Closeout",
    eyebrow: "Final report",
    path: "/closeout",
  },
];

export function workflowContextFromUrl(url: URL): WorkflowContext {
  return {
    inspectionId: url.searchParams.get("inspectionId"),
    siteId: url.searchParams.get("siteId"),
  };
}

export function workflowQuery(context: WorkflowContext): string {
  const params = new URLSearchParams();
  if (context.inspectionId) params.set("inspectionId", context.inspectionId);
  if (context.siteId) params.set("siteId", context.siteId);
  const value = params.toString();
  return value ? `?${value}` : "";
}

export function workflowHref(
  step: WorkflowStep,
  context: WorkflowContext,
): string {
  return `${step.path}${workflowQuery(context)}${
    step.hash ? `#${step.hash}` : ""
  }`;
}

export function workflowStepIndex(url: URL, context: WorkflowContext): number {
  if (url.pathname.endsWith("/evidence")) return 2;
  if (url.pathname.endsWith("/closeout")) return 3;
  if (
    url.pathname.endsWith("/inspection") &&
    (context.inspectionId || context.siteId)
  ) return 1;
  return 0;
}

export function workflowStepState(
  index: number,
  activeIndex: number,
  context: WorkflowContext,
): "current" | "completed" | "ready" | "locked" {
  if (index === activeIndex) return "current";
  if (index < activeIndex) return "completed";
  if (index === 1) {
    return context.siteId || context.inspectionId ? "ready" : "locked";
  }
  if (index === 2) {
    return context.siteId || context.inspectionId ? "ready" : "locked";
  }
  if (index === 3) return context.inspectionId ? "ready" : "locked";
  return "ready";
}
