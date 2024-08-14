import { z } from 'zod';

export const pdfQuerySchema = z.object({
  name: z.string(),
  url: z.string().url(),
});
