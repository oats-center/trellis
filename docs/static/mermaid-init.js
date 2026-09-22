(() => {
  const BASE = document.currentScript?.dataset.base || "";

  const isExternal = (href) =>
    href.startsWith("http://") ||
    href.startsWith("https://") ||
    href.startsWith("mailto:") ||
    href.startsWith("#") ||
    href.startsWith("//");

  const currentDesignDirectory = () => {
    const normalized = location.pathname.replace(/\/+$/, "");
    const segments = normalized.split("/").filter(Boolean);
    return `/${segments.slice(0, -1).join("/")}`;
  };

  const normalizeHref = (href) => {
    if (isExternal(href)) return href;

    let resolvedPath = href;

    if (href.startsWith("design/")) {
      resolvedPath = `/${href}`;
    } else if (href.startsWith("/design/")) {
      resolvedPath = href;
    } else if (href.startsWith("./") || href.startsWith("../")) {
      resolvedPath =
        new URL(href, `https://example.test${currentDesignDirectory()}/`)
          .pathname;
    }

    resolvedPath = resolvedPath.replace(/\.md(?=([?#]|$))/i, "");
    resolvedPath = resolvedPath.replace(/(?:^|\/)README(?=([?#]|$))/i, "");

    if (BASE && !resolvedPath.startsWith(BASE)) {
      resolvedPath = `${BASE}${resolvedPath}`;
    }

    return resolvedPath;
  };

  const rewriteLinks = (definition) =>
    definition.replace(
      /(?<![A-Za-z0-9_])((?:\.{1,2}\/|\/?design\/)[^\s"'<>`\]]+?\.md(?:#[^\s"'<>`]*)?)/g,
      (match) => normalizeHref(match),
    );

  const clamp = (value) => Math.min(255, Math.max(0, value));

  const srgbChannel = (value) => {
    const normalized = value <= 0.0031308
      ? 12.92 * value
      : 1.055 * value ** (1 / 2.4) - 0.055;
    return Math.round(clamp(normalized * 255));
  };

  const oklchToHex = (value) => {
    const match = value.match(
      /oklch\(\s*([\d.]+)%?\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*[\d.]+)?\s*\)/i,
    );
    if (!match) {
      return value;
    }

    const l = Number(match[1]) > 1 ? Number(match[1]) / 100 : Number(match[1]);
    const c = Number(match[2]);
    const h = (Number(match[3]) * Math.PI) / 180;

    const a = c * Math.cos(h);
    const b = c * Math.sin(h);

    const l_ = l + 0.3963377774 * a + 0.2158037573 * b;
    const m_ = l - 0.1055613458 * a - 0.0638541728 * b;
    const s_ = l - 0.0894841775 * a - 1.2914855480 * b;

    const l3 = l_ ** 3;
    const m3 = m_ ** 3;
    const s3 = s_ ** 3;

    const r = 4.0767416621 * l3 - 3.3077115913 * m3 + 0.2309699292 * s3;
    const g = -1.2684380046 * l3 + 2.6097574011 * m3 - 0.3413193965 * s3;
    const blue = -0.0041960863 * l3 - 0.7034186147 * m3 + 1.7076147010 * s3;

    const toHexChannel = (channel) =>
      srgbChannel(Math.min(1, Math.max(0, channel)));

    return `#${
      [toHexChannel(r), toHexChannel(g), toHexChannel(blue)]
        .map((n) => n.toString(16).padStart(2, "0"))
        .join("")
    }`;
  };

  const toHex = (value) => {
    if (!value) return value;

    if (value.startsWith("oklch(")) {
      return oklchToHex(value);
    }

    if (value.startsWith("#")) {
      return value;
    }

    const probe = document.createElement("span");
    probe.style.color = value;
    probe.style.display = "none";
    document.body.appendChild(probe);
    const resolved = getComputedStyle(probe).color;
    probe.remove();

    if (resolved.startsWith("oklch(")) {
      return oklchToHex(resolved);
    }

    const match = resolved.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/i);
    if (!match) {
      return value;
    }

    const [, r, g, b] = match.map(Number);
    return `#${
      [r, g, b]
        .map((n) => n.toString(16).padStart(2, "0"))
        .join("")
    }`;
  };

  const themeVars = () => {
    const styles = getComputedStyle(document.documentElement);
    const base100 = toHex(
      styles.getPropertyValue("--color-base-100").trim() || "#111827",
    );
    const base200 = toHex(
      styles.getPropertyValue("--color-base-200").trim() || "#1f2937",
    );
    const base300 = toHex(
      styles.getPropertyValue("--color-base-300").trim() || "#334155",
    );
    const content = toHex(
      styles.getPropertyValue("--color-base-content").trim() || "#e5e7eb",
    );
    const primary = toHex(
      styles.getPropertyValue("--color-primary").trim() || "#38bdf8",
    );
    const secondary = toHex(
      styles.getPropertyValue("--color-secondary").trim() || "#94a3b8",
    );

    return {
      background: "transparent",
      fontFamily: "inherit",
      primaryColor: base100,
      secondaryColor: base200,
      tertiaryColor: base300,
      primaryTextColor: content,
      secondaryTextColor: content,
      tertiaryTextColor: content,
      lineColor: base300,
      textColor: content,
      mainBkg: base100,
      nodeBkg: base100,
      nodeBorderColor: base300,
      clusterBkg: base200,
      clusterBorderColor: base300,
      edgeLabelBackground: base100,
      titleColor: content,
      actorBkg: base200,
      actorBorder: base300,
      actorTextColor: content,
      noteBkg: base200,
      noteBorderColor: base300,
      noteTextColor: content,
      activationBkgColor: primary,
      activationTextColor: content,
      labelBoxBkgColor: base200,
      labelBoxBorderColor: base300,
      labelTextColor: content,
      signalColor: secondary,
    };
  };

  const getMermaid = () =>
    window.mermaid || window.__esbuild_esm_mermaid_nm?.mermaid ||
    window.__mermaid;

  const loadMermaid = async () => {
    for (let attempt = 0; attempt < 50; attempt += 1) {
      const mermaid = getMermaid();
      if (mermaid) {
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: "strict",
          theme: "base",
          useMaxWidth: false,
          fontSize: 14,
          sequence: {
            actorFontSize: 14,
            noteFontSize: 14,
            messageFontSize: 14,
          },
          themeVariables: themeVars(),
        });

        return mermaid;
      }

      await new Promise((resolve) => setTimeout(resolve, 50));
    }

    throw new Error("Mermaid library not loaded");
  };

  let observer = null;
  let scheduled = false;
  let rendering = false;

  const render = async () => {
    if (rendering) return;
    rendering = true;
    observer?.disconnect();

    try {
      const mermaid = await loadMermaid();
      const blocks = document.querySelectorAll("pre > code.language-mermaid");

      for (const codeBlock of blocks) {
        const pre = codeBlock.parentElement;
        if (!pre || pre.dataset.mermaidRendered === "true") continue;

        const definition = codeBlock.textContent?.trim();
        if (!definition) continue;

        try {
          const randomId = typeof crypto.randomUUID === "function"
            ? crypto.randomUUID().replaceAll("-", "")
            : `${Date.now()}${Math.random().toString(16).slice(2)}`;
          const id = `mermaid-${randomId}`;
          const { svg } = await mermaid.render(id, rewriteLinks(definition));
          const container = document.createElement("div");
          container.className = "my-6 overflow-x-auto not-prose";
          container.dataset.mermaidRendered = "true";
          container.innerHTML = svg;

          const diagram = container.querySelector("svg");
          if (diagram) {
            diagram.style.maxWidth = "none";
            diagram.style.width = "auto";
            diagram.style.height = "auto";
            diagram.style.display = "block";
          }

          pre.replaceWith(container);
        } catch (error) {
          console.error("Failed to render Mermaid diagram", error);
        }
      }
    } finally {
      rendering = false;
      if (observer) {
        observer.observe(document.body, { childList: true, subtree: true });
      }
    }
  };

  const schedule = () => {
    if (scheduled) return;
    scheduled = true;
    queueMicrotask(async () => {
      scheduled = false;
      await render();
    });
  };

  const start = () => {
    if (!document.body) return;
    observer = new MutationObserver(schedule);
    observer.observe(document.body, { childList: true, subtree: true });
    schedule();
  };

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start, { once: true });
  } else {
    start();
  }

  window.addEventListener("pageshow", schedule);
})();
