import type { APIRoute } from 'astro';
import { buildRss } from '@/lib/feeds';

export const GET: APIRoute = () => buildRss('en');
