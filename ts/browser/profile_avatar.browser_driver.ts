import assert from "node:assert/strict";
import { chromium } from "playwright";
import { z } from "zod";

const application = z.url().parse(process.env.TRELLIS_TEST_AVATAR_APPLICATION);
const avatarOrigin = z.url().parse(process.env.TRELLIS_TEST_AVATAR_ORIGIN);
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  const requests: string[] = [];
  page.on("request", (request) => requests.push(request.url()));
  await page.goto(application);
  await page
    .getByRole("img", { name: "Profile photo" })
    .evaluate(async (image) => {
      if (!(image instanceof HTMLImageElement))
        throw new Error("Expected image");
      await image.decode();
      assertDimensions(image);
      function assertDimensions(image: HTMLImageElement) {
        if (image.naturalWidth !== 128 || image.naturalHeight !== 128)
          throw new Error("Wrong sanitized image dimensions");
      }
    });
  assert(
    requests.some((url) => url.startsWith(`${avatarOrigin}/profile/avatars/`)),
  );
  assert(
    requests.every((url) =>
      [new URL(application).origin, new URL(avatarOrigin).origin].includes(
        new URL(url).origin,
      ),
    ),
  );
} finally {
  await browser.close();
}
