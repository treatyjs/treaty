import { defineConfig } from 'vite';
import { angular } from './tools/vite';
import { AngularRoutingPlugin } from './tools/vite/routing';
import { splitVendorChunkPlugin } from 'vite';

export default defineConfig({
    plugins: [
        AngularRoutingPlugin({
            redirectTo: 'post/1'
        }),
        angular(),
        splitVendorChunkPlugin()
    ],
    
});