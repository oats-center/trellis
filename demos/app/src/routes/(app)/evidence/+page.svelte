<script lang="ts">
  import { ulid } from "ulid";
  import { TransferGrantSchema } from "@oats-center/trellis";
  import { Value } from "typebox/value";
  import { onDestroy, onMount } from "svelte";
  import { resolve } from "$app/paths";
  import { page } from "$app/state";
  import { SvelteMap } from "svelte/reactivity";
  import { formatBytes, formatDateTimeWithAge, formatPercent } from "$lib/format";
  import { getTrellis } from "$lib/trellis";

  type EvidenceUploadResponse = { evidenceId: string; key: string; size: number; disposition: string };
  type EvidenceRecord = {
    evidenceId: string;
    key: string;
    size: number;
    contentType?: string;
    evidenceType: string;
    fileName?: string;
    uploadedAt: string;
  };
  type GalleryItem = EvidenceRecord & {
    previewUrl?: string;
    previewing?: boolean;
    previewError?: string;
  };
  type TransferEvent = { transfer: { transferredBytes: number } };

  const trellis = getTrellis();
  const evidenceType = "field-photo";
  const evidenceTypeLabel = "Field photo";
  const evidencePageSize = 3;
  const listPage = { page: { limit: 50 } };

  type CloseoutRoute = "/closeout" | `/closeout?${string}`;

  let files = $state<FileList>();
  let running = $state(false);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let transferredBytes = $state(0);
  let result = $state<EvidenceUploadResponse | null>(null);
  let gallery = $state.raw<GalleryItem[]>([]);
  let inFlightFile = $state<{ name: string; size: number } | null>(null);
  let uploadModalOpen = $state(false);
  let evidencePage = $state(0);
  let pendingDeleteKey = $state<string | null>(null);
  let deletingKey = $state<string | null>(null);
  let mounted = false;
  let galleryRequestId = 0;
  let uploadRunId = 0;
  let nextDownloadRunId = 0;
  const downloadRunIds = new SvelteMap<string, number>();

  let selectedFile = $derived(files?.item(0) ?? null);
  let payloadBytes = $derived(inFlightFile?.size ?? selectedFile?.size ?? 0);
  let payloadName = $derived(inFlightFile?.name ?? selectedFile?.name ?? "No file selected");
  let transferPercent = $derived(payloadBytes > 0 ? Math.min(100, (transferredBytes / payloadBytes) * 100) : 0);
  let galleryPageCount = $derived(Math.max(1, Math.ceil(gallery.length / evidencePageSize)));
  let visibleGallery = $derived(gallery.slice(evidencePage * evidencePageSize, evidencePage * evidencePageSize + evidencePageSize));
  let visibleGalleryStart = $derived(gallery.length === 0 ? 0 : evidencePage * evidencePageSize + 1);
  let visibleGalleryEnd = $derived(Math.min(gallery.length, (evidencePage + 1) * evidencePageSize));
  let activeContext = $derived.by((): string | null => {
    const inspectionId = page.url.searchParams.get("inspectionId");
    const siteId = page.url.searchParams.get("siteId");
    if (inspectionId && siteId) return `${inspectionId} at ${siteId}`;
    return inspectionId ?? siteId;
  });
  let contextQuery = $derived.by((): string => {
    const params = new URLSearchParams();
    const inspectionId = page.url.searchParams.get("inspectionId");
    const siteId = page.url.searchParams.get("siteId");
    if (inspectionId) params.set("inspectionId", inspectionId);
    if (siteId) params.set("siteId", siteId);
    const value = params.toString();
    return value ? `?${value}` : "";
  });
  let closeoutRoute = $derived(`/closeout${contextQuery}` as CloseoutRoute);

  onMount(() => {
    mounted = true;
    void refreshGallery(true);
  });

  onDestroy(() => {
    mounted = false;
    galleryRequestId += 1;
    uploadRunId += 1;
    downloadRunIds.clear();
    clearPreviewUrls();
  });

  function safeFileName(fileName: string): string {
    return fileName.replace(/[^a-zA-Z0-9._-]+/g, "-").replace(/^-+|-+$/g, "") || "image";
  }

  function revokePreviewUrl(item: GalleryItem): void {
    if (item.previewUrl) {
      URL.revokeObjectURL(item.previewUrl);
    }
  }

  function clearPreviewUrls(): void {
    for (const item of gallery) {
      revokePreviewUrl(item);
    }
    gallery = gallery.map(({ previewUrl: _previewUrl, previewing: _previewing, previewError: _previewError, ...item }) => item);
  }

  function updateGalleryItem(key: string, patch: Partial<GalleryItem>): void {
    gallery = gallery.map((item) => item.key === key ? { ...item, ...patch } : item);
  }

  function clearSelectedFile(): void {
    files = new DataTransfer().files;
  }

  function openUploadModal(): void {
    if (!running) {
      result = null;
      transferredBytes = 0;
    }
    uploadModalOpen = true;
  }

  function setEvidencePage(page: number): void {
    evidencePage = Math.max(0, Math.min(galleryPageCount - 1, page));
    void loadVisiblePreviews();
  }

  async function loadVisiblePreviews(): Promise<void> {
    await Promise.all(
      visibleGallery
        .filter((item) => item.contentType?.startsWith("image/") && !item.previewUrl && !item.previewing)
        .map((item) => downloadPreview(item.key)),
    );
  }

  async function startUpload(): Promise<void> {
    const file = selectedFile;
    if (!file) {
      error = "Choose an image before uploading.";
      return;
    }

    running = true;
    error = null;
    transferredBytes = 0;
    result = null;
    inFlightFile = { name: file.name, size: file.size };
    const runId = ++uploadRunId;

    try {
      const evidenceId = ulid();
      const fileName = safeFileName(file.name);
      const key = `evidence/${evidenceId}-${fileName}`;
      const bytes = new Uint8Array(await file.arrayBuffer());
      if (!mounted || runId !== uploadRunId) return;
      const upload = await trellis.evidenceUpload({
          key,
          contentType: file.type || "application/octet-stream",
          evidenceType,
          metadata: { evidenceId, evidenceType, fileName: file.name },
        })
        .transfer(bytes)
        .onTransfer((event: TransferEvent) => {
          if (!mounted || runId !== uploadRunId) return;
          transferredBytes = event.transfer.transferredBytes;
        })
        .start()
        .orThrow();

      if (!mounted || runId !== uploadRunId) return;
      const completed = await upload.wait().orThrow();
      if (!mounted || runId !== uploadRunId) return;
      result = completed.terminal.output ?? null;
      clearSelectedFile();
      await refreshGallery(true);
    } catch (cause) {
      if (!mounted || runId !== uploadRunId) return;
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (!mounted || runId !== uploadRunId) return;
      running = false;
      inFlightFile = null;
    }
  }

  async function refreshGallery(downloadLatest: boolean): Promise<void> {
    const requestId = ++galleryRequestId;
    downloadRunIds.clear();
    refreshing = true;
    error = null;
    clearPreviewUrls();

    try {
      const list = await trellis.evidenceList({ ...listPage, prefix: "evidence/" }).orThrow();
      if (!mounted || requestId !== galleryRequestId) return;
      gallery = list.items
        .map((item) => ({ ...item, size: Number(item.size) }))
        .slice()
        .sort((left: EvidenceRecord, right: EvidenceRecord) => Date.parse(right.uploadedAt) - Date.parse(left.uploadedAt));
      evidencePage = 0;

      if (downloadLatest) {
        await loadVisiblePreviews();
      }
    } catch (cause) {
      if (!mounted || requestId !== galleryRequestId) return;
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (!mounted || requestId !== galleryRequestId) return;
      refreshing = false;
    }
  }

  async function downloadPreview(key: string): Promise<void> {
    const runId = ++nextDownloadRunId;
    downloadRunIds.set(key, runId);
    const existing = gallery.find((item) => item.key === key);
    if (!existing) {
      return;
    }

    revokePreviewUrl(existing);
    updateGalleryItem(key, { previewUrl: undefined, previewError: undefined, previewing: true });

    try {
      const download = await trellis.evidenceDownload({ key }).orThrow();
      const transfer = Value.Parse(TransferGrantSchema, download.transfer);
      if (transfer.direction !== "receive") throw new Error("Evidence download returned a send transfer grant.");
      const bytes = await trellis.transfer(transfer).bytes().orThrow();
      if (!mounted || downloadRunIds.get(key) !== runId) return;
      const latest = gallery.find((item) => item.key === key);
      const contentType = latest?.contentType ?? "application/octet-stream";
      const body = new ArrayBuffer(bytes.byteLength);
      new Uint8Array(body).set(bytes);
      const previewUrl = URL.createObjectURL(new Blob([body], { type: contentType }));
      updateGalleryItem(key, { previewUrl, previewing: false });
    } catch (cause) {
      if (!mounted || downloadRunIds.get(key) !== runId) return;
      updateGalleryItem(key, {
        previewError: cause instanceof Error ? cause.message : String(cause),
        previewing: false,
      });
    } finally {
      if (downloadRunIds.get(key) === runId) {
        downloadRunIds.delete(key);
      }
    }
  }

  async function deleteEvidence(key: string): Promise<void> {
    deletingKey = key;
    error = null;

    try {
      await trellis.evidenceDelete({ key }).orThrow();
      if (!mounted) return;
      const deleted = gallery.find((item) => item.key === key);
      if (deleted) revokePreviewUrl(deleted);
      gallery = gallery.filter((item) => item.key !== key);
      pendingDeleteKey = null;
      evidencePage = Math.min(evidencePage, Math.max(0, Math.ceil(gallery.length / evidencePageSize) - 1));
      await loadVisiblePreviews();
    } catch (cause) {
      if (!mounted) return;
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (!mounted) return;
      deletingKey = null;
    }
  }
</script>

<svelte:head>
  <title>Evidence Locker · Field Inspection Desk</title>
</svelte:head>

<section class="page-sheet rounded-box p-5 sm:p-7">
  <div class="flex flex-col gap-6">
  <header class="pb-1">
    <div class="flex min-w-0 flex-col gap-5 lg:flex-row lg:items-end lg:justify-between">
      <div class="min-w-0 space-y-3">
        <div class="trellis-kicker">Workflow step 2</div>
        <h1 class="break-words text-2xl font-black tracking-tight md:text-3xl">Verify field evidence</h1>
        <p class="max-w-3xl break-words text-sm text-base-content/70">
          Review stored photo evidence before closeout. Uploading more evidence is a side task when the chain of custody needs support.
        </p>
        {#if activeContext}
          <p class="source-label">Continuing {activeContext}</p>
        {/if}
      </div>
    </div>
    <p class="capability-note mt-4">
      <strong>Operation + RPC:</strong> Evidence.Upload + Evidence.List + Evidence.Download + Evidence.Delete
    </p>
  </header>

  <div class="workflow-progress-strip grid gap-3 border-y border-base-300/80 py-4 text-sm md:grid-cols-4" aria-label="Inspection workflow steps">
    <div class="workflow-progress-item">
      <span class="workflow-step-index" aria-hidden="true">1</span>
      <span class="min-w-0"><strong class="block">Inspection</strong><span class="text-base-content/64">Context selected</span></span>
    </div>
    <div class="workflow-progress-item" aria-current="step">
      <span class="workflow-step-index" aria-hidden="true">2</span>
      <span class="min-w-0"><strong class="block">Evidence</strong><span class="text-base-content/64">Verify photos</span></span>
    </div>
    <div class="workflow-progress-item">
      <span class="workflow-step-index" aria-hidden="true">3</span>
      <span class="min-w-0"><strong class="block">Closeout</strong><span class="text-base-content/64">Publish report</span></span>
    </div>
    <div class="workflow-progress-item">
      <span class="workflow-step-index" aria-hidden="true">4</span>
      <span class="min-w-0"><strong class="block">Final report</strong><span class="text-base-content/64">Closeout</span></span>
    </div>
  </div>

  {#if error}
    <div role="alert" class="alert alert-error"><span>{error}</span></div>
  {/if}

  <section class="section-rule pt-6">
    <div class="flex flex-col gap-5">
      {#if refreshing && gallery.length === 0}
        <div class="alert" role="status"><span>Loading stored evidence records from Evidence.List.</span></div>
      {:else if gallery.length === 0}
        <div class="flex min-w-0 flex-wrap items-center justify-between gap-3 border-t border-base-300/80 py-3">
          <p class="text-sm text-base-content/64">No records returned from Evidence.List.</p>
          <div class="flex flex-wrap items-center gap-2">
            <button class="btn btn-outline btn-sm" onclick={openUploadModal}>Upload evidence</button>
            <button class="btn btn-square btn-outline btn-sm" onclick={() => refreshGallery(true)} disabled={refreshing} aria-label="Refresh evidence locker">
              {#if refreshing}
                <span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
              {:else}
                <svg aria-hidden="true" class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M21 12a9 9 0 0 1-15.5 6.2" />
                  <path d="M3 12A9 9 0 0 1 18.5 5.8" />
                  <path d="M18.5 2v3.8H22" />
                  <path d="M5.5 22v-3.8H2" />
                </svg>
              {/if}
            </button>
          </div>
        </div>
      {:else}
        <div class="flex min-w-0 flex-wrap items-center justify-between gap-3 border-t border-base-300/80 py-3">
          <div class="min-w-0">
            <p class="source-label">Evidence gallery</p>
            <p class="mt-1 text-sm text-base-content/64">Live records returned from Evidence.List.</p>
          </div>
          <div class="flex flex-wrap items-center gap-2">
            <button class="btn btn-outline btn-sm" onclick={openUploadModal}>Upload evidence</button>
            <div class="join" aria-label="Evidence pages">
              <button class="btn join-item btn-outline btn-sm" onclick={() => setEvidencePage(evidencePage - 1)} disabled={evidencePage === 0} aria-label="Show newer evidence">‹</button>
              <span class="btn join-item btn-sm cursor-default bg-base-200" aria-label={`Page ${evidencePage + 1} of ${galleryPageCount}`}>{evidencePage + 1} / {galleryPageCount}</span>
              <button class="btn join-item btn-outline btn-sm" onclick={() => setEvidencePage(evidencePage + 1)} disabled={evidencePage >= galleryPageCount - 1} aria-label="Show older evidence">›</button>
            </div>
            <button class="btn btn-square btn-outline btn-sm" onclick={() => refreshGallery(true)} disabled={refreshing} aria-label="Refresh evidence locker">
              {#if refreshing}
                <span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
              {:else}
                <svg aria-hidden="true" class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M21 12a9 9 0 0 1-15.5 6.2" />
                  <path d="M3 12A9 9 0 0 1 18.5 5.8" />
                  <path d="M18.5 2v3.8H22" />
                  <path d="M5.5 22v-3.8H2" />
                </svg>
              {/if}
            </button>
          </div>
        </div>
        <div class="grid min-w-0 gap-x-6 gap-y-0 md:grid-cols-2 xl:grid-cols-3">
          {#each visibleGallery as item (item.key)}
            <article class="min-w-0 border-b border-base-300/70 py-5 md:pr-4 xl:[&:nth-last-child(-n+3)]:border-b-0">
              <figure class="bg-base-200/45 p-3">
                {#if item.previewUrl}
                  <img class="h-44 w-full rounded object-cover" src={item.previewUrl} alt={item.fileName ?? item.key} />
                {:else if item.previewing}
                  <div class="preview-placeholder flex h-44 w-full flex-col justify-end rounded border border-base-300/80 p-3 text-sm text-base-content/62" role="status">
                    <span>Loading image preview</span>
                    <progress class="progress progress-accent mt-3 w-full" aria-label={`Loading preview for ${item.fileName ?? item.key}`}></progress>
                  </div>
                {:else}
                  <div class="flex h-44 w-full items-center justify-center rounded border border-dashed border-base-300 text-sm text-base-content/60">
                    Preview unavailable
                  </div>
                {/if}
              </figure>
              <div class="flex min-w-0 flex-col gap-3 pt-4">
                <div class="min-w-0">
                  <h3 class="break-words font-medium">{item.fileName ?? item.key}</h3>
                  <p class="break-all font-mono text-xs text-base-content/60">{item.key}</p>
                </div>
                <div class="grid min-w-0 grid-cols-[auto_minmax(0,1fr)] gap-2 text-xs text-base-content/58">
                  <span>Size</span><span>{formatBytes(item.size)}</span>
                  <span>Uploaded</span><span class="min-w-0 break-words">{formatDateTimeWithAge(item.uploadedAt)}</span>
                  <span>Details</span><span class="min-w-0 break-words">{item.contentType ?? "unknown"} · {item.evidenceType}</span>
                </div>
                {#if item.previewError}
                  <div role="alert" class="alert alert-error py-2 text-sm"><span>{item.previewError}</span></div>
                {/if}
                <div class="flex flex-wrap justify-end gap-2">
                  {#if !item.previewUrl && !item.previewing}
                    <button class="btn btn-outline btn-sm" onclick={() => downloadPreview(item.key)} aria-label={`Retry preview for ${item.fileName ?? item.key}`}>
                      Retry preview
                    </button>
                  {/if}
                  {#if pendingDeleteKey === item.key}
                    <button class="btn btn-error btn-sm" onclick={() => deleteEvidence(item.key)} disabled={deletingKey === item.key}>Confirm delete</button>
                    <button class="btn btn-ghost btn-sm" onclick={() => pendingDeleteKey = null} disabled={deletingKey === item.key}>Keep upload</button>
                  {:else}
                    <button class="btn btn-outline btn-sm" onclick={() => pendingDeleteKey = item.key} disabled={deletingKey === item.key} aria-label={`Delete evidence upload ${item.fileName ?? item.key}`}>
                      Delete upload
                    </button>
                  {/if}
                  </div>
              </div>
            </article>
          {/each}
        </div>
        <p class="border-t border-base-300/80 pt-3 text-sm text-base-content/64">Showing {visibleGalleryStart}-{visibleGalleryEnd} of {gallery.length}</p>
      {/if}

      <div class="next-action-rail px-1 py-4">
        <p class="source-label">Primary continuation</p>
        <div class="mt-3 flex flex-wrap gap-3">
          <a class="btn btn-accent btn-sm" href={resolve(closeoutRoute)}>Next: closeout</a>
          <button class="btn btn-ghost btn-sm" onclick={openUploadModal}>Upload additional evidence</button>
        </div>
      </div>
    </div>
  </section>

  {#if uploadModalOpen}
    <div class="operation-modal-backdrop fixed inset-0 z-50 flex items-center justify-center p-4" role="dialog" aria-modal="true" aria-labelledby="upload-evidence-heading">
      <section class="demo-modal-surface relative max-h-[92vh] w-full max-w-3xl overflow-y-auto rounded-box p-6 sm:p-8">
        <button
          class="btn btn-ghost btn-sm btn-circle absolute right-4 top-4"
          onclick={() => uploadModalOpen = false}
          disabled={running}
          aria-label="Close upload evidence dialog"
        >
          ×
        </button>
        <div class="flex min-w-0 flex-col gap-6">
          <div class="flex min-w-0 flex-wrap items-start justify-between gap-4">
            <div class="min-w-0 space-y-2">
              <p class="trellis-kicker">Evidence.Upload</p>
              <h2 id="upload-evidence-heading" class="break-words text-2xl font-black tracking-tight">Upload photo evidence</h2>
              <p class="break-words text-sm text-base-content/68">
                Choose an inspection photo. The transfer keeps the selected file as the in-flight payload until storage confirms it.
              </p>
            </div>
          </div>

          <label class="form-control gap-2">
            <span class="label-text font-medium">Image evidence</span>
            <input class="file-input file-input-bordered w-full" type="file" accept="image/*" bind:files disabled={running} />
          </label>

          <div class="flex min-w-0 flex-wrap items-center gap-3">
            <button class="btn btn-accent" onclick={startUpload} disabled={running || selectedFile === null}>
              {running ? "Sending transfer..." : "Upload photo evidence"}
            </button>
            {#if selectedFile}
              <button class="btn btn-ghost" onclick={clearSelectedFile} disabled={running}>Clear selection</button>
            {/if}
          </div>
          <p class="break-words text-xs text-base-content/58">
            {payloadName} · {formatBytes(payloadBytes)} · record type: {evidenceTypeLabel}
          </p>

          <div class="border-y border-base-300/80 bg-base-200/35 px-1 py-4">
            <div class="flex min-w-0 flex-wrap items-center justify-between gap-3 text-sm">
              <span class="min-w-0 break-words font-medium">{running ? `Uploading ${payloadName}` : payloadName}</span>
              <span class="text-base-content/62">{formatPercent(transferPercent)}</span>
            </div>
            <progress class="progress progress-accent mt-3 w-full" value={transferPercent} max="100" aria-label="Upload transfer progress"></progress>
            <p class="mt-2 break-words text-xs text-base-content/56">
              {formatBytes(transferredBytes)} sent of {formatBytes(payloadBytes)}
            </p>
          </div>

          {#if !running && !result}
            <div class="alert"><span>Start a photo upload to see transfer progress.</span></div>
          {:else if result}
            <details class="border-y border-base-300/80 bg-base-200/25 px-1 py-3 text-sm">
              <summary class="cursor-pointer font-semibold text-base-content/70">Evidence.Upload details</summary>
              <div class="mt-3 overflow-x-auto text-base-content/66">
                <table class="table table-sm executive-table min-w-[30rem]">
                  <tbody>
                    <tr><th scope="row">Evidence id</th><td class="break-words font-mono text-xs">{result.evidenceId}</td></tr>
                    <tr><th scope="row">Key</th><td class="break-words font-mono text-xs">{result.key}</td></tr>
                    <tr><th scope="row">Recorded size</th><td>{formatBytes(result.size)}</td></tr>
                    <tr><th scope="row">Disposition</th><td>{result.disposition}</td></tr>
                  </tbody>
                </table>
              </div>
            </details>
          {/if}

          {#if result && !running}
            <div class="flex flex-wrap justify-end gap-3">
              <button class="btn btn-accent" onclick={() => uploadModalOpen = false}>Return to evidence</button>
            </div>
          {/if}
        </div>
      </section>
    </div>
  {/if}
  </div>
</section>
