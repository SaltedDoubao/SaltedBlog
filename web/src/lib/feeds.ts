import { getPosts, getSettingsCached } from '@/lib/api';
import { langPrefix, type Lang } from '@/i18n';

function escapeXml(input: string): string {
  return input
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&apos;');
}

export function siteUrl(): string {
  return (
    (typeof process !== 'undefined' && process.env?.PUBLIC_SITE_URL) ||
    import.meta.env.PUBLIC_SITE_URL ||
    'http://localhost:4321'
  ).replace(/\/$/, '');
}

export async function buildRss(lang: Lang): Promise<Response> {
  const [settings, postsRes] = await Promise.all([
    getSettingsCached(),
    getPosts({ lang, page: 1, pageSize: 20 }),
  ]);
  const base = siteUrl();
  const title = settings[`site_title_${lang}`] || 'SaltedBlog';
  const description = settings[`description_${lang}`] || '';
  const channelLink = `${base}${langPrefix(lang)}/`;

  const items = postsRes.items
    .map((post) => {
      const link = `${base}${langPrefix(lang)}/posts/${encodeURIComponent(post.slug)}`;
      const pubDate = post.published_at ? new Date(post.published_at).toUTCString() : '';
      return [
        '    <item>',
        `      <title>${escapeXml(post.title)}</title>`,
        `      <link>${escapeXml(link)}</link>`,
        `      <guid isPermaLink="true">${escapeXml(link)}</guid>`,
        post.summary ? `      <description>${escapeXml(post.summary)}</description>` : '',
        pubDate ? `      <pubDate>${pubDate}</pubDate>` : '',
        '    </item>',
      ]
        .filter(Boolean)
        .join('\n');
    })
    .join('\n');

  const xml = `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>${escapeXml(title)}</title>
    <link>${escapeXml(channelLink)}</link>
    <description>${escapeXml(description)}</description>
    <language>${lang === 'zh' ? 'zh-CN' : 'en'}</language>
${items}
  </channel>
</rss>
`;

  return new Response(xml, {
    headers: { 'Content-Type': 'application/rss+xml; charset=utf-8' },
  });
}
