import { apiBaseUrl } from '$lib/config';
import type { RequestHandler } from './$types';

const proxy: RequestHandler = async ({ request, params, url }) => {
  const upstream = new URL(`/api/${params.path ?? ''}${url.search}`, apiBaseUrl);
  const headers = new Headers(request.headers);
  headers.delete('host');

  const response = await fetch(upstream, {
    method: request.method,
    headers,
    body: request.method === 'GET' || request.method === 'HEAD' ? undefined : request.body,
    duplex: 'half'
  } as RequestInit & { duplex: 'half' });

  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: response.headers
  });
};

export const GET = proxy;
export const POST = proxy;
export const PUT = proxy;
export const PATCH = proxy;
export const DELETE = proxy;

