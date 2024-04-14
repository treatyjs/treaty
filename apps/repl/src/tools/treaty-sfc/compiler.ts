import { R3InputMetadata } from '@angular/compiler';
import { Printer } from './printer'
import { basename, dirname, extname } from 'path';
import type { Plugin } from 'vite';
import { parseTreaty, parseTreatyAndGroup } from './treaty/parser';

export function loadEsmModule<T>(modulePath: string | URL): Promise<T> {
	return new Function('modulePath', `return import(modulePath);`)(
		modulePath
	) as Promise<T>;
}
function extractFileName(filePath: string) {

	const fileName = basename(filePath, extname(filePath));

	const dirPath = dirname(filePath);

	const lastFolderName = basename(dirPath);

	// if (lastFolderName !== "pages") {

	//   return `${lastFolderName}-${fileName}`;
	// }

	return fileName;
}

function toPascalCase(fileName: string): string {
	return fileName

		.replace(/[^a-zA-Z0-9\.]+/g, ' ')
		.split('.')
		.map(part =>
			part.split(' ')
				.map(word => word.charAt(0).toUpperCase() + word.slice(1).toLowerCase())
				.join('')
		).join('');
}

function toCamelCase(fileName: string): string {
	return fileName

		.replace(/[^a-zA-Z0-9\.]+/g, ' ')
		.split('.')
		.map((part, index) =>
			part.split(' ')
				.map((word, wordIndex) =>
					wordIndex === 0 && index === 0
						? word.toLowerCase()
						: word.charAt(0).toUpperCase() + word.slice(1).toLowerCase()
				).join('')
		).join('');
}

function toHyphenCase(fileName: string): string {
	return fileName

		.replace(/([a-z])([A-Z])/g, '$1-$2')
		.replace(/[\s_.]+/g, '-')
		.toLowerCase()
		.replace(/-+/g, '-');
}
function extractImportStrings(code: string) {
	const regex = /import\s+?(?:(?:(?:[\w*\s{},]*)\s+from\s+?)|)(?:(?:".*?")|(?:'.*?'))[\s]*?(?:;|$|)/g;
	const matches = code.match(regex);
	return matches || [];
}

function removeImportsFromCode(code: string) {
	const regex = /import\s+?(?:(?:(?:[\w*\s{},]*)\s+from\s+?)|)(?:(?:".*?")|(?:'.*?'))[\s]*?(?:;|$|)/g;
	return code.replace(regex, '').trim();
}

function createWrapper(wrapperName: string, code: string, strExpression: string) {

	const patterns = [
		/let\s+([\w,\s]+)\s*=/g,
		/const\s+([\w,\s]+)\s*=/g,
		/var\s+([\w,\s]+)\s*=/g,
		/function\s+(\w+)/g,
		/([\w]+)\s*=\s*\(?.*\)?\s*=>/g,
	];

	const names = new Set();

	patterns.forEach(pattern => {
		let match;
		while ((match = pattern.exec(code)) !== null) {

			match[1].split(',').forEach(name => names.add(name.trim()));
		}
	});

	const returnObjectString = `return { ${[...names].join(', ')} };`;

	const wrappedFunction = `
  ${extractImportStrings(code).join('\n')}
  import * as i0 from "@angular/core";
  import * as i1 from "@angular/common";
  function ${wrapperName}() {
  ${removeImportsFromCode(code)}
  ${returnObjectString}
}

${wrapperName}.ɵfac = function ${wrapperName}_Factory(t) { return (t || ${wrapperName})(); };
${wrapperName}.ɵcmp = ${strExpression}

export default ${wrapperName};
`;

	return wrappedFunction;
}

export class LiteralMapEntry {
	constructor(public key: string, public value: typeof import('@angular/compiler').Expression, public quoted: boolean) { }
	isEquivalent(e: LiteralMapEntry): boolean {
		return this.key === e.key && (this.value as any).isEquivalent(e.value);
	}

	clone(): LiteralMapEntry {
		return new LiteralMapEntry(this.key, (this.value as any).clone(), this.quoted);
	}
}

function extractKeysFromJS(jsString: string, objectName: string) {
	const objectPattern = new RegExp(`const ${objectName} = {([^}]+)}`);
	const objectMatch = jsString.match(objectPattern);
	if (!objectMatch) {
		console.log(`Object "${objectName}" not found in the JS string.`);
		return [];
	}
	const objectContent = objectMatch[1];
	return objectContent.split(',').map(property => {
		const [key] = property.split(':').map(part => part.trim());
		return key;
	});
}

function replaceSpreadWithBindings(htmlStrings: string[], objectName: string, keys: string[]) {
	return htmlStrings.map(htmlString => {
		let modifiedHtmlString = htmlString;
		const spreadPattern = new RegExp(`{\\.\\.\\.${objectName}}`, 'g');
		if (spreadPattern.test(htmlString)) {
			const replacements = keys.map(key => `[${key}]="${objectName}.${key}()"`).join(' ');
			modifiedHtmlString = modifiedHtmlString.replace(spreadPattern, replacements);
		}
		return modifiedHtmlString;
	});
}

async function findInputAndOutputAssignments(code: string) {

	// Check for 'input' and 'output' imports
	const importCheck = /import\s+{[^}]*\b(input|output)\b[^}]*}\s+from\s+['"]@angular\/core['"]/;
	const hasImport = importCheck.test(code);

	if (!hasImport) {
		return { inputs: {}, outputs: {} }; // Exit if necessary imports are not found
	}

	// Regex pattern to match 'input()', 'input.required()', and 'output()' with arguments
	const pattern = /(?:let|const)\s+(\w+)\s*=\s*(input(?:\.required)?|output)\(([^)]*)\)/g;

	const assignments = {
		inputs: {} as {
			[field: string]: R3InputMetadata;
		},
		outputs: {} as {
			[field: string]: string;
		}
	};

	let match;
	while ((match = pattern.exec(code)) !== null) {
		const [_, variableName, functionName, functionArguments] = match;
		const isRequired = functionName.includes('.required');

		if (functionName.startsWith('input')) {
			assignments.inputs[variableName] = {
				transformFunction: null,
				classPropertyName: variableName,
				bindingPropertyName: variableName,
				required: isRequired,
				isSignal: true
			};
		} else if (functionName === 'output') {
			assignments.outputs[variableName] = variableName;
		}
	}

	return assignments;
}



export const treatySFC: () => Plugin = () => {
	let compiler: typeof import('@angular/compiler');
	let printer: ReturnType<typeof Printer>
	return {
		name: 'vite-plugin-template-dev',
		enforce: 'pre',
		async buildStart() {
			compiler = await loadEsmModule<
				typeof import('@angular/compiler')
			>('@angular/compiler');

			printer = Printer(compiler);

		},
		config() {
			return {
				esbuild: false,
			};
		},
		async transform(code, id) {

			if (id.endsWith('.treaty')) {

				

				let { html: htmlContent, javascript: jsTsContent, style: cssContent } = await parseTreatyAndGroup(code)
				console.log(cssContent)
				const importRegex = /import\s+(?:\{\s*([^}]+)\s*\}|\* as (\w+)|(\w+))(?:\s+from\s+)?(?:".*?"|'.*?')[\s]*?(?:;|$)/g;
				let match;
				const imports = [];
				while ((match = importRegex.exec(jsTsContent))) {
					const matchedImport = match[1] || match[2] || match[3];
					imports.push(...matchedImport.split(',').map(name => name.trim()));
				}

				let modifiedCode = code;

				const addToDeclatoration: any[] = []
				imports.forEach(importName => {
					const tagStartRegex = new RegExp(`<${importName}`, 'g');
					const tagEndRegex = new RegExp(`</${importName}>`, 'g');
					if (tagStartRegex.test(modifiedCode)) {
						if (!addToDeclatoration.includes(importName)) {
							addToDeclatoration.push(importName)
						}
						htmlContent = htmlContent.map((code) => code.replace(tagStartRegex, `<${toHyphenCase(importName)} `).replace(tagEndRegex, `</${toHyphenCase(importName)}>`));
					}
				});

				const fileName = extractFileName(id);
				const selector = `${toHyphenCase(fileName)}, ${toCamelCase(fileName)}, ${toPascalCase(fileName)}`

				let updatedHtmlStrings = [...htmlContent];
				const spreadPattern = /\.\.\.(\w+)\s*}/;
				htmlContent.forEach(htmlString => {
					const spreadMatch = htmlString.match(spreadPattern);
					if (spreadMatch) {
						const objectName = spreadMatch[1];
						const keys = extractKeysFromJS(jsTsContent, objectName);
						updatedHtmlStrings = replaceSpreadWithBindings(updatedHtmlStrings, objectName, keys);
					}
				});

				const CMP_NAME = toCamelCase(fileName)

				const angularTemplate = compiler.parseTemplate(updatedHtmlStrings.join('\n'), id)

				const { inputs, outputs } = await findInputAndOutputAssignments(jsTsContent)

				const constantPool = new compiler.ConstantPool();
				const out = compiler.compileComponentFromMetadata(
					{
						name: CMP_NAME,
						isStandalone: true,
						selector,
						host: {
							attributes: {},
							listeners: {},
							properties: {},
							specialAttributes: {},
							useTemplatePipeline: true,
						},
						rawImports: addToDeclatoration as any,
						inputs,
						outputs: outputs,
						lifecycle: {
							usesOnChanges: false,
						},
						hostDirectives: null,
						declarations: [],
						declarationListEmitMode: 0,
						deferBlockDepsEmitMode: 1,
						deferBlocks: new Map(),
						deferrableDeclToImportDecl: new Map(),
						deps: [],
						animations: null,
						deferrableTypes: new Map(),
						i18nUseExternalIds: false,
						interpolation: compiler.DEFAULT_INTERPOLATION_CONFIG,
						isSignal: true,
						providers: null,
						queries: [],
						styles: cssContent,
						template: angularTemplate,
						encapsulation: compiler.ViewEncapsulation.Emulated,
						exportAs: null,
						fullInheritance: false,
						changeDetection: null,
						relativeContextFilePath: 'template.html',
						type: {
							value: new compiler.WrappedNodeExpr(CMP_NAME),
							type: new compiler.WrappedNodeExpr(CMP_NAME),
						},
						typeArgumentCount: 0,
						typeSourceSpan: null!,
						useTemplatePipeline: true,
						usesInheritance: false,
						viewProviders: null,
						viewQueries: [],
					},
					constantPool,
					compiler.makeBindingParser(compiler.DEFAULT_INTERPOLATION_CONFIG)
				);

				(out.expression as any).args[0].entries.push(new LiteralMapEntry('dependencies', new compiler.LiteralArrayExpr(
					addToDeclatoration.map(val => new compiler.WrappedNodeExpr(val))) as any, false))

				const strExpression = out.expression.visitExpression(
					new printer.Printer(),
					new printer.Context(false)
				);
				const treatyIvy = createWrapper(toCamelCase(fileName), jsTsContent || '', strExpression)
				return treatyIvy;
			}
			return code;
		},
	};
}