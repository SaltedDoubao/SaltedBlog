import type { APIRoute } from 'astro';
import { getSitemapData } from '@/lib/api';
import { siteUrl } from '@/lib/feeds';

export const GET: APIRoute = async () => {
  const base = siteUrl();
  const data = await getSitemapData();

  const urls: { loc: string; lastmod?: string }[] = [];

  const staticPaths = [
    '/',
    '/posts',
    '/archive',
    '/tags',
    '/categories',
    '/series',
    '/friends',
    '/about',
    '/search',
  ];
  for (const p of staticPaths) {
    urls.push({ loc: `${base}${p === '/' ? '' : p}` || base });
    urls.push({ loc: `${base}/en${p === '/' ? '' : p}` });
  }

  for (const post of data.posts) {
    const prefix = post.lang === 'zh' ? '' : '/en';
    urls.push({
      loc: `${base}${prefix}/posts/${encodeURIComponent(post.slug)}`,
      lastmod: post.updated_at ? new Date(post.updated_at).toISOString() : undefined,
    });
  }
  for (const slug of data.categories) {
    urls.push({ loc: `${base}/categories/${slug}` });
    urls.push({ loc: `${base}/en/categories/${slug}` });
  }
  for (const slug of data.tags) {
    urls.push({ loc: `${base}/tags/${slug}` });
    urls.push({ loc: `${base}/en/tags/${slug}` });
  }
  for (const slug of data.series) {
    urls.push({ loc: `${base}/series/${slug}` });
    urls.push({ loc: `${base}/en/series/${slug}` });
  }

  const body = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls
  .map(
    (u) =>
      `  <url><loc>${u.loc}</loc>${u.lastmod ? `<lastmod>${u.lastmod}</lastmod>` : ''}</url>`
  )
  .join('\n')}
</urlset>
`;

  return new Response(body, {
    headers: { 'Content-Type': 'application/xml; charset=utf-8' },
  });
};
