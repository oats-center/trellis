import { qrcode } from "@libs/qrcode";

const QUIET_ZONE_MODULES = 4;

function getQrCell(
  matrix: boolean[][],
  x: number,
  y: number,
): boolean {
  return matrix[y]?.[x] ?? false;
}

function getQrCharacter(
  top: boolean,
  bottom: boolean,
): string {
  if (top && bottom) {
    return "█";
  }

  if (top) {
    return "▀";
  }

  if (bottom) {
    return "▄";
  }

  return " ";
}

/**
 * Render a compact terminal QR code that packs two QR rows into one text row.
 */
export function renderCompactQr(content: string | URL): void {
  const matrix = qrcode(content, { output: "array" });
  const width = matrix[0]?.length ?? 0;
  const height = matrix.length;
  const totalWidth = width + QUIET_ZONE_MODULES * 2;
  const totalHeight = height + QUIET_ZONE_MODULES * 2;
  const lines: string[] = [];

  for (let y = 0; y < totalHeight; y += 2) {
    let line = "";

    for (let x = 0; x < totalWidth; x += 1) {
      const top = getQrCell(
        matrix,
        x - QUIET_ZONE_MODULES,
        y - QUIET_ZONE_MODULES,
      );
      const bottom = getQrCell(
        matrix,
        x - QUIET_ZONE_MODULES,
        y + 1 - QUIET_ZONE_MODULES,
      );
      line += getQrCharacter(top, bottom);
    }

    lines.push(line);
  }

  console.log(lines.join("\n"));
}
