import '@angular/compiler';
// import './treaty-utilities/mock-create-histogram';
import './treaty-utilities/mock-zone';

import { Elysia, t } from 'elysia';
import { IndexHtmlTransform } from 'vite';

import { Surreal } from 'surrealdb.node';

import { APP_BASE_HREF } from '@angular/common';
import { CommonEngine } from '@angular/ssr';
import bootstrap from './src/main.server';

const db = new Surreal();
await db.connect('memory');
await db.use({ ns: 'test', db: 'test' });

const port = process.env['PORT'] || 5555;

const commonEngine = new CommonEngine({
  enablePerformanceProfiler: true,
});

const app = new Elysia()
  .derive(({ request: { url } }) => {
    const _url = new URL(url);

    return {
      protocol: _url.protocol.split(':')[0],
      originalUrl: _url.pathname + _url.search,
      baseUrl: '',
    };
  })
  .group('/api', (api) => {
    return api
      .get('/id/:id', ({ params: { id } }) => ({ data: `Post with id: ${id}` }))
      .get('/example', () => `just an example`)
      .post('/form', ({ body }) => body, {
        body: t.Object({
          strField: t.String(),
          numbField: t.Number(),
        }),
      });
  })
  .get('*.*', async ({ originalUrl }) => {
    const file = Bun.file(`./src/${originalUrl}`);

    return new Response(Buffer.from(await file.arrayBuffer()), {
      headers: {
        'Content-Type': file.type,
      },
    });
  })
  .get('*', async ({ originalUrl, baseUrl, protocol, headers }) => {
    if (originalUrl.includes('.')) {
      const file = Bun.file(`.78src/${originalUrl}`);

      return new Response(Buffer.from(await file.arrayBuffer()), {
        headers: {
          'Content-Type': file.type,
        },
      });
    }

    const cacheHit = await db.select(`url:\`${originalUrl}\``);

    if (cacheHit) {
      return new Response(cacheHit.content, {
        headers: {
          'Content-Type': 'text/html',
        },
      });
    }

    try {
      console.log(`${protocol}://${headers['host']}${originalUrl}`);
      let template = await Bun.file('./' + 'index.html').text()
      const _html = await commonEngine.render({
        bootstrap,
        document: template,
        url: `${protocol}://${headers['host']}${originalUrl}`,
        publicPath: './src',
        providers: [{ provide: APP_BASE_HREF, useValue: '' }],
      });

      console.log(_html);

      await db.create(`url:\`${originalUrl}\``, {
        content: _html,
      });

      return new Response(_html, {
        headers: {
          'Content-Type': 'text/html',
        },
      });
    } catch (error) {
      console.log(error);

      return new Response('Missing page', {
        headers: {
          'Content-Type': 'text/html',
        },
      });
    }
  })

export type App = typeof app;
export default app;
