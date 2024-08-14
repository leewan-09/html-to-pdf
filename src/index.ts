import { Hono } from 'hono';
import puppeteer from 'puppeteer';
import { env } from './env';
import { zValidator } from '@hono/zod-validator';
import { pdfQuerySchema } from './schema';

const app = new Hono();

app.post('/', zValidator('json', pdfQuerySchema), async (c) => {
  const { name, url } = c.req.valid('json');
  console.log(`Generating PDF for ${name} from ${url}`);

  const browser = await puppeteer.launch({
    headless: true,
    args: ['--no-sandbox'],
  });
  const page = await browser.newPage();
  await page.goto(url);
  const pdf = await page.pdf({ format: 'A4', printBackground: true });
  await browser.close();

  return new Response(pdf, {
    headers: {
      'Content-Type': 'application/pdf',
      'Content-Disposition': `inline; filename="${name}.pdf"`,
    },
  });
});

export default {
  port: env.PORT,
  fetch: app.fetch,
};
