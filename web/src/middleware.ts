import { defineMiddleware } from 'astro:middleware';
import { getMe } from '@/lib/api';

/** 后台会话守卫：未登录访问 /admin/* 时重定向到登录页 */
export const onRequest = defineMiddleware(async (context, next) => {
  const { pathname } = context.url;
  const isAdminRoute = pathname === '/admin' || pathname.startsWith('/admin/');
  const isLoginPage = pathname === '/admin/login' || pathname === '/admin/login/';

  if (isAdminRoute && !isLoginPage) {
    const me = await getMe(context.request.headers.get('cookie'));
    if (!me) {
      return context.redirect('/admin/login');
    }
    context.locals.adminUser = me.username;
  }

  return next();
});
