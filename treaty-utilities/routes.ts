import { plugin, type BunPlugin } from "bun";
import { relative } from 'path'
import { AngularRoutingPlugin, AngularRouting, routes } from '../tools/vite/routing'
export const AngularRoutesRunTime: BunPlugin = {
  name: "Angular Bun routing loader",
  setup(build) {
    build.module("virtual:angular-routing:bun", () => {
        const router = new Bun.FileSystemRouter({
            style: "nextjs",
            dir: "src/app/pages",
          });

          const routingMapper: RoutingMapper = {imports: [], routes:[]} 

          Object.keys(router.routes).forEach((key, index) => {
            routingMapper.imports.push(`import {RouterMeta as r${index}} from '${router.routes[key]}'`);
            routingMapper.routes.push(`{
                path: '${key.replace(/\[(.*?)\]/g, ':$1').substring(1)}',
                loadComponent: () => import('${relative(__dirname, router.routes[key]).replace('../', '')}'),
                ...r${index}
            }`);
          })
            

        const content = `
        ${
            routingMapper.imports.join(`\n`)
        }

        export const bunRoutes = [
          ${routingMapper.routes.join(',\n')}
        ]
      `;
      console.log('content', content)
        return {
          contents:content,
          loader: "ts",
        };
      });
  },
};


export const AngularRoutesBuild:(routingInfo?: AngularRouting) => BunPlugin = (routingInfo = {}) => ({
  name: "Angular Bun routing loader",
  setup(build) {
    const routePath = routingInfo.filePath ?? 'src/app/app.routes.ts'
    const pagesPath = routingInfo.pagesPath ?? 'src/app/pages';
    build.onLoad({filter: new RegExp(routePath, 'i') }, (args) => {
      return {
        contents: routes({
          ...routingInfo,
          filePath: routePath,
          pagesPath: pagesPath
        } as any),
        loader: 'js'
      } 

  
    });
  },
});

plugin(AngularRoutesBuild({redirectTo: 'post/1'}));