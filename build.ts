import { Angular } from "treaty-utilities/angular.ts";
// import { AngularRoutesBuild } from "treaty-utilities/routes";

async function build() {
	const file = Bun.file("./src/index.html");
	await Bun.build({
		entrypoints: ['./src/main.ts'],
		outdir: './dist/treaty/browser',
		define: {
			ngDevMode: 'false',
			ngJitMode: 'false',
			ngI18nClosureMode: 'false',
		},
		format: 'esm',
		splitting: false,
		sourcemap: 'none',
		minify: false,
		target: "browser",
		plugins: [
			Angular,
			// AngularPlugin
			//plugins[1],
		]
	});
	await Bun.write("./dist/treaty/browser/index.html", file);

}

build()