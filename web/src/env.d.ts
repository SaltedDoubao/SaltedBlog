/// <reference types="astro/client" />

declare namespace App {
  interface Locals {
    /** 由后台守卫中间件写入的当前管理员用户名 */
    adminUser?: string;
  }
}
