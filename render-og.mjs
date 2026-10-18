// Exact brand typography and real app imagery, rendered at social-card size.
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
const require = createRequire(new URL('../app/ui/package.json', import.meta.url));
const { chromium } = require('@playwright/test');
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1200, height: 630 }, deviceScaleFactor: 1 });
  await page.goto(new URL('./social/og.html', import.meta.url).href);
  await page.evaluate(async () => {
    await document.fonts.ready;
    await Promise.all([...document.images].map(img => img.decode()));
  });
  const path = fileURLToPath(new URL('./assets/og-gmclip-v2.jpg', import.meta.url));
  await page.screenshot({ path, type: 'jpeg', quality: 94 });
  console.log(path);
} finally {
  await browser.close();
}
