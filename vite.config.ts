import { defineConfig } from 'vite';
import { angular } from './tools/vite/angular';
import { AngularRoutingPlugin } from './tools/vite/routing';
import { splitVendorChunkPlugin } from 'vite';
import {devServer} from './tools/vite/elysia/dev-server'


export default defineConfig({
    server: {
        port: 5555
    },
    plugins: [
        devServer({
            entry: './server.ts',
        }),
        AngularRoutingPlugin({
            redirectTo: 'post/1'
        }),
        angular(),
        splitVendorChunkPlugin()
    ],
    
});
