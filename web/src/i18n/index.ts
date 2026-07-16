export type Lang = 'zh' | 'en';

export const LANGS: Lang[] = ['zh', 'en'];

const dict = {
  zh: {
    'nav.home': '首页',
    'nav.posts': '文章',
    'nav.archive': '归档',
    'nav.series': '系列',
    'nav.friends': '友链',
    'nav.about': '关于',
    'nav.search': '搜索',
    'nav.menu': '菜单',
    'home.eyebrow': 'PERSONAL BLOG / OVER THE FRONTIER',
    'home.latest': '最新文章',
    'home.latest.sub': '按时间顺序记录的技术与生活',
    'home.viewAll': '查看全部文章',
    'home.stats.posts': '文章',
    'home.stats.categories': '分类',
    'home.stats.tags': '标签',
    'posts.title': '全部文章',
    'posts.eyebrow': 'POSTS / INDEX',
    'posts.filter.category': '分类',
    'posts.filter.tag': '标签',
    'posts.filter.series': '系列',
    'posts.empty': '暂无文章',
    'posts.count': '篇文章',
    'post.toc': '目录',
    'post.updated': '更新于',
    'post.views': '次阅读',
    'post.prev': '上一篇',
    'post.next': '下一篇',
    'post.series.part': '本文属于系列',
    'post.translation': 'Read in English',
    'post.tags': '标签',
    'archive.title': '归档',
    'archive.eyebrow': 'ARCHIVE / TIMELINE',
    'archive.total': '共',
    'archive.unit': '篇',
    'tags.title': '标签',
    'tags.eyebrow': 'TAGS / INDEX',
    'categories.title': '分类',
    'categories.eyebrow': 'CATEGORIES / INDEX',
    'series.title': '系列',
    'series.eyebrow': 'SERIES / COLLECTIONS',
    'search.title': '搜索',
    'search.eyebrow': 'SEARCH / FULLTEXT',
    'search.placeholder': '输入关键词，回车搜索…',
    'search.result': '条结果',
    'search.empty': '没有找到相关内容',
    'friends.title': '友情链接',
    'friends.eyebrow': 'FRIENDS / LINKS',
    'friends.empty': '暂无友链',
    'about.title': '关于',
    'about.eyebrow': 'ABOUT / PROFILE',
    'footer.rss': 'RSS 订阅',
    'footer.builtWith': '基于 Astro + Rust 构建',
    'notfound.title': '页面不存在',
    'notfound.desc': '你访问的页面可能已被移除或从未存在。',
    'notfound.back': '返回首页',
    'comments.title': '评论',
  },
  en: {
    'nav.home': 'Home',
    'nav.posts': 'Posts',
    'nav.archive': 'Archive',
    'nav.series': 'Series',
    'nav.friends': 'Friends',
    'nav.about': 'About',
    'nav.search': 'Search',
    'nav.menu': 'Menu',
    'home.eyebrow': 'PERSONAL BLOG / OVER THE FRONTIER',
    'home.latest': 'Latest Posts',
    'home.latest.sub': 'Notes on tech and life, in chronological order',
    'home.viewAll': 'View all posts',
    'home.stats.posts': 'POSTS',
    'home.stats.categories': 'CATEGORIES',
    'home.stats.tags': 'TAGS',
    'posts.title': 'All Posts',
    'posts.eyebrow': 'POSTS / INDEX',
    'posts.filter.category': 'Category',
    'posts.filter.tag': 'Tag',
    'posts.filter.series': 'Series',
    'posts.empty': 'No posts yet',
    'posts.count': 'posts',
    'post.toc': 'Contents',
    'post.updated': 'Updated',
    'post.views': 'views',
    'post.prev': 'Previous',
    'post.next': 'Next',
    'post.series.part': 'Part of series',
    'post.translation': '阅读中文版',
    'post.tags': 'Tags',
    'archive.title': 'Archive',
    'archive.eyebrow': 'ARCHIVE / TIMELINE',
    'archive.total': 'Total',
    'archive.unit': 'posts',
    'tags.title': 'Tags',
    'tags.eyebrow': 'TAGS / INDEX',
    'categories.title': 'Categories',
    'categories.eyebrow': 'CATEGORIES / INDEX',
    'series.title': 'Series',
    'series.eyebrow': 'SERIES / COLLECTIONS',
    'search.title': 'Search',
    'search.eyebrow': 'SEARCH / FULLTEXT',
    'search.placeholder': 'Type keywords and press Enter…',
    'search.result': 'results',
    'search.empty': 'Nothing found',
    'friends.title': 'Friends',
    'friends.eyebrow': 'FRIENDS / LINKS',
    'friends.empty': 'No links yet',
    'about.title': 'About',
    'about.eyebrow': 'ABOUT / PROFILE',
    'footer.rss': 'RSS Feed',
    'footer.builtWith': 'Built with Astro + Rust',
    'notfound.title': 'Page Not Found',
    'notfound.desc': 'The page you are looking for may have been removed or never existed.',
    'notfound.back': 'Back to home',
    'comments.title': 'Comments',
  },
} as const;

export type DictKey = keyof (typeof dict)['zh'];

export function t(lang: Lang, key: DictKey): string {
  return dict[lang][key] ?? dict.zh[key] ?? key;
}

/** 语言前缀：zh 无前缀，en 为 /en */
export function langPrefix(lang: Lang): string {
  return lang === 'zh' ? '' : '/en';
}

/** 构造带语言前缀的站内路径 */
export function localePath(lang: Lang, path: string): string {
  const p = path.startsWith('/') ? path : `/${path}`;
  return `${langPrefix(lang)}${p}` || '/';
}

/** 把当前路径映射到另一语言的对应路径（默认规则） */
export function altLangPath(lang: Lang, pathname: string): string {
  if (lang === 'zh') {
    return `/en${pathname === '/' ? '' : pathname}` || '/en';
  }
  const stripped = pathname.replace(/^\/en/, '');
  return stripped === '' ? '/' : stripped;
}

export function otherLang(lang: Lang): Lang {
  return lang === 'zh' ? 'en' : 'zh';
}

/** 取分类/标签/系列的本地化名称 */
export function localName(
  lang: Lang,
  item: { name_zh: string; name_en: string } | null | undefined
): string {
  if (!item) return '';
  return lang === 'zh' ? item.name_zh : item.name_en;
}

export function formatDate(lang: Lang, iso: string | null | undefined): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}
