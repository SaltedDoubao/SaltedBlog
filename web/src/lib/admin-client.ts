/// 后台页面浏览器端共用工具（在 <script> 中 import）

export async function adminFetch<T = unknown>(path: string, init?: RequestInit): Promise<T> {
  const unsafe = init?.method && !['GET', 'HEAD', 'OPTIONS'].includes(init.method.toUpperCase());
  const csrfName = location.protocol === 'https:' ? '__Host-sb_csrf' : 'sb_csrf';
  const csrf = document.cookie
    .split(';')
    .map((part) => part.trim())
    .find((part) => part.startsWith(`${csrfName}=`))
    ?.slice(csrfName.length + 1);
  const headers = new Headers(init?.headers);
  if (init?.body && !(init.body instanceof FormData) && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  if (unsafe && csrf) headers.set('X-CSRF-Token', csrf);
  const res = await fetch(path, {
    ...init,
    headers,
  });
  if (res.status === 401) {
    location.href = '/admin/login';
    throw new Error('unauthorized');
  }
  if (!res.ok) {
    let message = `HTTP ${res.status}`;
    try {
      const body = await res.json();
      if (body?.error) message = body.error;
    } catch {
      /* ignore */
    }
    throw new Error(message);
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  return (text ? JSON.parse(text) : undefined) as T;
}

export async function stepUp(): Promise<void> {
  const password = window.prompt('高危操作需要再次验证。请输入当前密码：');
  if (!password) throw new Error('已取消二次验证');
  const code = window.prompt('请输入验证器中的 6 位动态码：');
  if (!code) throw new Error('已取消二次验证');
  await adminFetch('/api/auth/step-up', {
    method: 'POST',
    body: JSON.stringify({ password, code }),
  });
}

let toastWrap: HTMLElement | null = null;

export function toast(message: string, kind: 'ok' | 'error' = 'ok'): void {
  if (!toastWrap) {
    toastWrap = document.createElement('div');
    toastWrap.className = 'toast-wrap';
    document.body.appendChild(toastWrap);
  }
  const el = document.createElement('div');
  el.className = `toast toast--${kind}`;
  el.textContent = message;
  toastWrap.appendChild(el);
  setTimeout(() => el.remove(), 3200);
}

export function escapeHtml(input: string): string {
  return input
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

export function qs<T extends HTMLElement>(selector: string): T {
  const el = document.querySelector<T>(selector);
  if (!el) throw new Error(`missing element: ${selector}`);
  return el;
}

export function confirmDanger(message: string): boolean {
  return window.confirm(message);
}
