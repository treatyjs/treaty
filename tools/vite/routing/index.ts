import { Route } from '@angular/router';
import type { Plugin } from 'vite';
import { relative, dirname } from 'path'
export type AngularRouting = {
  redirectTo?: string;
  filePath?: string
  pagesPath?: string;
}

type RoutingMapper = { imports: string[], routes: string[] }
export type RoutingMeta = Omit<Route, 'path' | 'matcher' | 'loadComponent' | 'component' | 'redirectTo' | 'children' | 'loadChildren'>

const AngularRoutingPlugin: (routingInfo?: AngularRouting) => Plugin = (routingInfo = {}) => {

  return {
    name: 'vite-plugin-angular-routing',
    enforce: 'pre',
    transform(code, id) {

      const routePath = routingInfo.filePath ?? 'src/app/app.routes.ts'
      const pagesPath = routingInfo.pagesPath ?? 'src/app/pages';

      if (!id.includes(routePath)) {
        return;
      }

      const genCode = routes({
        ...routingInfo,
        filePath: routePath,
        pagesPath
      } as any);

      console.log(genCode);
      return {
        code: genCode
      }
    },
  }
};

export function routes(routingInfo: Required<AngularRouting>) {
  const router = new Bun.FileSystemRouter({
    style: "nextjs",
    dir: routingInfo.pagesPath,
  });

  console.log('routes', router.routes)

  const routingMapper: RoutingMapper = { imports: [], routes: [] }

  Object.keys(router.routes).forEach((key, index) => {
    routingMapper.imports.push(`import {routerMeta as r${index}} from './${relative(dirname(routingInfo.filePath), router.routes[key])}'`);
    routingMapper.routes.push(`{
    path: '${key.replace(/\[(.*?)\]/g, ':$1').substring(1)}',
    loadComponent: () => import('./${relative(dirname(routingInfo.filePath), router.routes[key])}'),
    ...r${index},
  }`);
  })



  const redirectRoute = `
  {
    path: '',
    redirectTo: '${routingInfo.redirectTo}',
    pathMatch: 'full',
  },
`

  const content = `
${routingMapper.imports.join(`\n`)
    }

export const routes = [
  ${routingInfo.redirectTo ? redirectRoute : ''}
  ${routingMapper.routes.join(',\n')}
]
`;
  return content;
}

export { AngularRoutingPlugin };