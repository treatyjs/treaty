import { Elysia } from 'elysia'
import type http from 'http'
import { minimatch } from 'minimatch'
import { type Plugin as VitePlugin, type ViteDevServer, type Connect, createViteRuntime } from 'vite'

export type DevServerOptions = {
  entry?: string
  export?: string
  injectClientScript?: boolean
  exclude?: (string | RegExp)[]
} 

export const defaultOptions: Required<DevServerOptions> = {
  entry: './src/index.ts',
  export: 'default',
  injectClientScript: true,
  exclude: [
    /.*\.ts$/,
    /.*\.tsx$/,
    /^\/@.+$/,
    /^\/favicon\.ico$/,
    /^\/static\/.+/,
    /^\/node_modules\/.*/,
  ],
}

export function devServer(options?: DevServerOptions): VitePlugin {
  const entry = options?.entry ?? defaultOptions.entry
  const plugin: VitePlugin = {
    name: 'vite-elysia-dev-server',
    configureServer: async (server) => {
      async function createMiddleware(server: ViteDevServer): Promise<Connect.HandleFunction> {
        return async function (
          req: http.IncomingMessage,
          res: http.ServerResponse,
          next: Connect.NextFunction
        ): Promise<void> {
          const exclude = options?.exclude ?? defaultOptions.exclude

          for (const pattern of exclude) {
            if (req.url) {
              if (pattern instanceof RegExp) {
                if (pattern.test(req.url)) {
                  return next()
                }
              } else if (minimatch(req.url?.toString(), pattern)) {
                return next()
              }
            }
          }

          let appModule;
          try {
            const runtime = await createViteRuntime(server);
            appModule = runtime.executeEntrypoint.bind(runtime);
          } catch (e) {
            return next(e);
          }
          const exportName = options?.export ?? defaultOptions.export;
          const mod = await appModule(entry);
          const app = mod[exportName] as Elysia;

          if (!app) {
            return next(new Error(`Failed to find a named export "${exportName}" from ${entry}`))
          }

          const response = await app.handle(convertIncomingMessageToRequest(req));

        
          if (!(response instanceof Response)) {
            throw response
          }

          if (
            options?.injectClientScript !== false &&
            response.headers.get('content-type')?.match(/^text\/html/)
          ) {
            const script = '<script>import("/@vite/client")</script>'
            const response = injectStringToResponse(req as any, script)!;
            returnResponse(response, res)
          }
          return returnResponse(response, res)
        }
      }

      server.middlewares.use(await createMiddleware(server))

    },
  }
  return plugin
}

function injectStringToResponse(response: Response, content: string) {
    const encoder = new TextEncoder();
    const newContent = encoder.encode(content);
    const headers = new Headers(response.headers);
    headers.delete('content-length');

    const originalBodyStream = response.body;
    const appendedStream = new ReadableStream({
        async start(controller) {
            if (originalBodyStream) {
                const reader = originalBodyStream.getReader();
                while (true) {
                    const { done, value } = await reader.read();
                    if (done) break;
                    controller.enqueue(value);
                }
            }
            controller.enqueue(newContent);
            controller.close();
        },
    });

    return new Response(appendedStream, {
        status: response.status,
        statusText: response.statusText,
        headers: headers,
    });
}


async function returnResponse(response: Response, res: http.ServerResponse) {
    res.statusCode = response.status;

    response.headers.forEach((value, key) => {
        res.setHeader(key, value);
    });

    if (response.body) {
        const reader = response.body.getReader();
        reader.read().then(function processText({ done, value }) {
            if (done) {
                res.end();
                return;
            }
            res.write(value);
            reader.read().then(processText);
        });
    } else {
        res.end();
    }
}


function convertIncomingMessageToRequest(req: http.IncomingMessage): Request {
    const headers = new Headers();
    Object.entries(req.headers).forEach(([key, value]) => {
      if (value) {
        headers.append(key, value as string);
      }
    });
  
    const body = req.method !== 'GET' && req.method !== 'HEAD' ? req : null;
  
    const url = new URL(req.url || '', `http://${req.headers.host}`);
  
    const requestInit: RequestInit = {
      method: req.method,
      headers: headers,
      body: body as any,
      redirect: 'follow',
    };
  
    return new Request(url.toString(), requestInit);
  }
