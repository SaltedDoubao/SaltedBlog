/// API 客户端：SSR 服务端直接访问 Rust API

const API_URL: string =
  (typeof process !== 'undefined' && process.env?.API_URL) ||
  import.meta.env.API_URL ||
  'http://127.0.0.1:8787';

export interface CategoryOut {
  id: number;
  slug: string;
  name_zh: string;
  name_en: string;
  count?: number;
}

export interface TagOut {
  id: number;
  slug: string;
  name_zh: string;
  name_en: string;
  count?: number;
}

export interface SeriesOut {
  id: number;
  slug: string;
  name_zh: string;
  name_en: string;
  count?: number;
  description_zh?: string | null;
  description_en?: string | null;
}

export interface PostListItem {
  id: number;
  group_id: string;
  lang: string;
  slug: string;
  title: string;
  summary: string | null;
  cover: string | null;
  status: string;
  category: CategoryOut | null;
  tags: TagOut[];
  series: SeriesOut | null;
  series_order: number | null;
  view_count: number;
  published_at: string | null;
  updated_at: string;
}

export interface PostListResponse {
  items: PostListItem[];
  total: number;
  page: number;
  page_size: number;
}

export interface TocItem {
  level: number;
  id: string;
  text: string;
}

export interface PostDetailResponse {
  post: PostListItem;
  content_html: string;
  toc: TocItem[];
  translations: { lang: string; slug: string; title: string }[];
  series_posts: {
    id: number;
    slug: string;
    title: string;
    series_order: number | null;
    current: boolean;
  }[];
  prev: { slug: string; title: string } | null;
  next: { slug: string; title: string } | null;
}

export interface ArchiveItem {
  slug: string;
  title: string;
  published_at: string | null;
  view_count: number;
}

export interface TaxonomyResponse {
  categories: CategoryOut[];
  tags: TagOut[];
  series: SeriesOut[];
}

export interface FriendItem {
  id: number;
  name: string;
  url: string;
  avatar: string | null;
  description: string | null;
  sort_order: number;
}

export type SettingsMap = Record<string, string>;

export interface SitemapData {
  posts: { lang: string; slug: string; updated_at: string; published_at: string | null }[];
  categories: string[];
  tags: string[];
  series: string[];
}

class ApiFetchError extends Error {
  constructor(
    public status: number,
    message: string
  ) {
    super(message);
  }
}

async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${API_URL}${path}`, init);
  if (!res.ok) {
    let message = `API ${res.status}`;
    try {
      const body = await res.json();
      if (body?.error) message = body.error;
    } catch {
      /* ignore */
    }
    throw new ApiFetchError(res.status, message);
  }
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

export function isNotFound(err: unknown): boolean {
  return err instanceof ApiFetchError && err.status === 404;
}

// ---- 公开接口 ----

export function getPosts(params: {
  lang: string;
  page?: number;
  pageSize?: number;
  category?: string;
  tag?: string;
  series?: string;
}): Promise<PostListResponse> {
  const q = new URLSearchParams({ lang: params.lang });
  if (params.page) q.set('page', String(params.page));
  if (params.pageSize) q.set('page_size', String(params.pageSize));
  if (params.category) q.set('category', params.category);
  if (params.tag) q.set('tag', params.tag);
  if (params.series) q.set('series', params.series);
  return apiFetch(`/api/posts?${q}`);
}

export function getPostDetail(lang: string, slug: string): Promise<PostDetailResponse> {
  return apiFetch(`/api/posts/${lang}/${encodeURIComponent(slug)}`);
}

export function getArchive(lang: string): Promise<{ items: ArchiveItem[] }> {
  return apiFetch(`/api/archive?lang=${lang}`);
}

export function getTaxonomy(lang: string): Promise<TaxonomyResponse> {
  return apiFetch(`/api/taxonomy?lang=${lang}`);
}

export function searchPosts(lang: string, q: string): Promise<{ items: PostListItem[]; q: string }> {
  return apiFetch(`/api/search?lang=${lang}&q=${encodeURIComponent(q)}`);
}

export function getFriends(): Promise<{ items: FriendItem[] }> {
  return apiFetch('/api/friends');
}

export function getSettings(): Promise<SettingsMap> {
  return apiFetch('/api/settings/public');
}

let settingsCache: { data: SettingsMap; at: number } | null = null;

/** 站点设置带 60s 内存缓存（每次 SSR 页面渲染都会用到） */
export async function getSettingsCached(): Promise<SettingsMap> {
  if (settingsCache && Date.now() - settingsCache.at < 60_000) {
    return settingsCache.data;
  }
  try {
    const data = await getSettings();
    settingsCache = { data, at: Date.now() };
    return data;
  } catch {
    return settingsCache?.data ?? {};
  }
}

export function getAbout(lang: string): Promise<{ html: string; toc: TocItem[] }> {
  return apiFetch(`/api/about?lang=${lang}`);
}

// ---- AI 日报 ----

export interface DigestItemOut {
  title_zh: string;
  title_en: string;
  summary_zh: string;
  summary_en: string;
  why_zh: string;
  why_en: string;
  source: string;
  url: string | null;
  importance: number;
  tags: string[];
  anchor: string;
}

export interface LatestDigest {
  date: string;
  slug: string;
  title_zh: string;
  title_en: string;
  summary_zh: string;
  summary_en: string;
  item_count: number;
  generated_at: string | null;
  items: DigestItemOut[];
}

/** 最新一期已发布的 AI 日报；无日报或接口异常时返回 null（主页显示空状态） */
export async function getLatestDigest(): Promise<LatestDigest | null> {
  try {
    const res = await apiFetch<{ digest: LatestDigest | null }>('/api/news/latest');
    return res.digest;
  } catch {
    return null;
  }
}

export function getSitemapData(): Promise<SitemapData> {
  return apiFetch('/api/sitemap');
}

/** 按 slug 查找分类/标签/系列（供过滤页做 404 判断），未找到返回 null */
export async function findTerm(
  lang: string,
  kind: 'category' | 'tag' | 'series',
  slug: string
): Promise<CategoryOut | TagOut | SeriesOut | null> {
  const taxonomy = await getTaxonomy(lang);
  const pool =
    kind === 'category' ? taxonomy.categories : kind === 'tag' ? taxonomy.tags : taxonomy.series;
  return pool.find((item) => item.slug === slug) ?? null;
}

/** 获取文章详情；404 时返回 null（供页面 frontmatter 做 rewrite 判断） */
export async function getPostDetailOrNull(
  lang: string,
  slug: string
): Promise<PostDetailResponse | null> {
  try {
    return await getPostDetail(lang, slug);
  } catch (err) {
    if (isNotFound(err)) return null;
    throw err;
  }
}

// ---- 管理端（SSR 转发 Cookie） ----

export function getMe(cookie: string | null): Promise<{ username: string } | null> {
  if (!cookie) return Promise.resolve(null);
  return apiFetch<{ username: string }>('/api/auth/me', {
    headers: { cookie },
  }).catch(() => null);
}
