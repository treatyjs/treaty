import { type R3InputMetadata, type R3QueryMetadata } from '@angular/compiler';
import ts from 'typescript';
import { Printer } from './printer';
import { Lexer } from './treaty/lexer';
import { Parser } from './treaty/parser';
import { Token, TokenType } from './treaty/token';

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

function extractImportStrings(sourceFile: ts.SourceFile) {
    const imports: string[] = [];
    function visit(node: ts.Node) {
        if (ts.isImportDeclaration(node)) {
            const importText = node.getText(sourceFile);
            imports.push(importText);
        }
        ts.forEachChild(node, visit);
    }
    visit(sourceFile);
    return imports;
}

function removeImportsFromCode(sourceFile: ts.SourceFile) {
    const statements = sourceFile.statements.filter(statement => !ts.isImportDeclaration(statement));
    const printer = ts.createPrinter();
    const newSourceFile = ts.factory.updateSourceFile(sourceFile, statements);
    return printer.printFile(newSourceFile);
}

function createWrapper(wrapperName: string, sourceFile: ts.SourceFile, strExpression: string, constantDeclarations: string) {
    const names = new Set<string>();

    function isTopLevelNode(node: ts.Node): boolean {
        return node.parent ? ts.isSourceFile(node.parent) : true;
    }

    function visit(node: ts.Node) {
        if (ts.isVariableStatement(node)) {
            if (!isTopLevelNode(node)) return
            node.declarationList.declarations.forEach(declaration => {
                if (ts.isIdentifier(declaration.name)) {
                    names.add(declaration.name.text);
                }
            });
        } else if (ts.isFunctionDeclaration(node) && node.name) {
            names.add(node.name.text);
        }
        ts.forEachChild(node, visit);
    }

    visit(sourceFile);

    const returnObjectString = `return { ${Array.from(names).join(', ')} };`;

    const wrappedFunction = `
  ${extractImportStrings(sourceFile).join('\n')}
  import * as i0 from "@angular/core";
  import * as i1 from "@angular/common";
  ${constantDeclarations}
  function ${wrapperName}() {
  ${removeImportsFromCode(sourceFile)}
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

function extractKeysFromJS(sourceFile: ts.SourceFile, objectName: string) {
    const keys: string[] = [];

    function visit(node: ts.Node) {
        if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === objectName) {
            if (node.initializer && ts.isObjectLiteralExpression(node.initializer)) {
                node.initializer.properties.forEach(property => {
                    if (ts.isPropertyAssignment(property) && ts.isIdentifier(property.name)) {
                        keys.push(property.name.text);
                    }
                });
            } else if (node.initializer && ts.isCallExpression(node.initializer)) {
                const returnType = getReturnType(node.initializer);
                if (returnType) {
                    keys.push(...returnType);
                }
            }
        } else if (ts.isFunctionDeclaration(node) && node.name && node.name.text === objectName) {
            const returnType = getReturnType(node);
            if (returnType) {
                keys.push(...returnType);
            }
        }
        ts.forEachChild(node, visit);
    }

    function getReturnType(node: ts.Node): string[] | null {
        if (ts.isCallExpression(node)) {
            const typeChecker = ts.createProgram(['temp.ts'], {}).getTypeChecker();
            const signature = typeChecker.getResolvedSignature(node);
            if (signature) {
                const returnType = typeChecker.getReturnTypeOfSignature(signature);
                if (returnType.isUnion()) {
                    return returnType.types.map(type => typeChecker.typeToString(type));
                }
            }
        } else if (ts.isFunctionDeclaration(node) && node.body) {
            const returnStatements = node.body.statements.filter(ts.isReturnStatement);
            if (returnStatements.length > 0) {
                const returnType = returnStatements[0].expression;
                if (returnType && ts.isObjectLiteralExpression(returnType)) {
                    return returnType.properties.map(property => (property.name as ts.Identifier).text);
                }
            }
        }
        return null;
    }

    visit(sourceFile);
    return keys;
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

async function findInputAndOutputAssignments(sourceFile: ts.SourceFile) {
    const assignments = {
        inputs: {} as { [field: string]: R3InputMetadata },
        outputs: {} as { [field: string]: string }
    };

    function visit(node: ts.Node) {
        if (ts.isVariableStatement(node)) {
            node.declarationList.declarations.forEach(declaration => {
                if (ts.isIdentifier(declaration.name) && declaration.initializer) {
                    const variableName = declaration.name.text;
                    if (ts.isCallExpression(declaration.initializer)) {
                        const expression = declaration.initializer.expression;
                        if (ts.isIdentifier(expression)) {
                            if (expression.text === 'input' || expression.text === 'output') {
                                const isInput = expression.text === 'input';
                                const isRequired = isInput && ts.isPropertyAccessExpression(declaration.initializer.expression) &&
                                    declaration.initializer.expression.name.text === 'required';

                                if (isInput) {
                                    assignments.inputs[variableName] = {
                                        transformFunction: null,
                                        classPropertyName: variableName,
                                        bindingPropertyName: variableName,
                                        required: isRequired,
                                        isSignal: true
                                    };
                                } else {
                                    assignments.outputs[variableName] = variableName;
                                }
                            }
                        }
                    }
                }
            });
        }
        ts.forEachChild(node, visit);
    }

    visit(sourceFile);
    return assignments;
}

async function findViewChildAndContentQueries(sourceFile: ts.SourceFile) {
    const queries = {
        viewQueries: [] as R3QueryMetadata[],
        contentQueries: [] as R3QueryMetadata[],
        constantDeclarations: [] as string[]
    };

    let constantIndex = 0;

    function addQuery(variableName: string, predicate: string, isViewQuery: boolean, isMulti: boolean) {
        const constantName = `_c${constantIndex++}`;
        queries.constantDeclarations.push(`const ${constantName} = [${predicate}];`);

        const query: R3QueryMetadata = {
            propertyName: variableName,
            predicate: [predicate],
            descendants: true,
            first: !isMulti,
            read: null,
            static: false,
            emitDistinctChangesOnly: true,
            isSignal: true
        };

        if (isViewQuery) {
            queries.viewQueries.push(query);
        } else {
            queries.contentQueries.push(query);
        }
    }

    function visit(node: ts.Node) {
        if (ts.isVariableStatement(node)) {
            node.declarationList.declarations.forEach(declaration => {
                if (ts.isIdentifier(declaration.name) && declaration.initializer && ts.isCallExpression(declaration.initializer)) {
                    const variableName = declaration.name.text;
                    const callExpression = declaration.initializer;
                    if (ts.isIdentifier(callExpression.expression)) {
                        const functionName = callExpression.expression.text;
                        const predicate = (callExpression.arguments[0] as any)?.text;
                        if (predicate) {
                            switch (functionName) {
                                case 'viewChild':
                                    addQuery(variableName, predicate, true, false);
                                    break;
                                case 'viewChildren':
                                    addQuery(variableName, predicate, true, true);
                                    break;
                                case 'contentChildren':
                                    addQuery(variableName, predicate, false, true);
                                    break;
                                case 'contentQuery':
                                    addQuery(variableName, predicate, false, false);
                                    break;
                            }
                        }
                    }
                }
            });
        }
        ts.forEachChild(node, visit);
    }

    visit(sourceFile);
    return queries;
}

export const treatyToIvy = async (code: string, id: string, compiler: typeof import('@angular/compiler'), extractFileName?: (file: string) => string, printer?: ReturnType<typeof Printer>) => {
    printer = printer ?? Printer(compiler);
    const lexer = new Lexer(code);
    const tokens: Token[] = [];
    let token;
    while ((token = lexer.nextToken())) {
        tokens.push(token);
    }
    const javascriptChunks: string[] = [];
    const htmlChunks: string[] = [];
    const cssChunks: string[] = [];
    const parser = new Parser(tokens)
    const ast = parser.parse();
    ast
    for (const node of ast.nodes) {
        switch (node.type) {
            case TokenType.JavaScript:
                javascriptChunks.push(node.value);
                break;
            case TokenType.HTML:
                htmlChunks.push(node.value);
                break;
            case TokenType.Style:
                cssChunks.push(node.value);
                break;
            default:
                break;
        }
    }
    const cssContent = cssChunks.map(
        (token) =>
          (token)
            .replaceAll('\n', '')
            .replaceAll('\t', '') as string)
    const jsTsContent = javascriptChunks.join('')
    console.log('jsTsContent', jsTsContent)
    console.log('html', htmlChunks)

    const sourceFile = ts.createSourceFile('temp.ts', jsTsContent, ts.ScriptTarget.Latest, true);
    const imports = extractImportStrings(sourceFile);

    let modifiedCode = code;

    let htmlContent = htmlChunks;

    const addToDeclatoration: any[] = []
    if (imports.length) {
        imports.forEach(importName => {
            const tagStartRegex = new RegExp(`<${importName}`, 'g');
            const tagEndRegex = new RegExp(`</${importName}>`, 'g');
            if (tagStartRegex.test(modifiedCode)) {
                if (!addToDeclatoration.includes(importName)) {
                    addToDeclatoration.push(importName)
                }
                htmlContent = htmlContent.map((code) => code.replace(tagStartRegex, `<${toHyphenCase(importName)} `).replace(tagEndRegex, `</${toHyphenCase(importName)}>`))
            }
        });
    }

    const fileName = extractFileName!(id);
    const selector = `${toHyphenCase(fileName)}, ${toCamelCase(fileName)}, ${toPascalCase(fileName)}`

    let updatedHtmlStrings = [...htmlContent];
    const spreadPattern = /\.\.\.(\w+)\s*}/;
    htmlContent.forEach(htmlString => {
        const spreadMatch = htmlString.match(spreadPattern);
        if (spreadMatch) {
            const objectName = spreadMatch[1];
            const keys = extractKeysFromJS(sourceFile, objectName);
            updatedHtmlStrings = replaceSpreadWithBindings(updatedHtmlStrings, objectName, keys);
        }
    });

    htmlContent = htmlContent.map((code) => code.replaceAll('\n', '').replaceAll('\r', '')
                        .replaceAll('\t', '').toString());

    const CMP_NAME = toCamelCase(fileName)
    console.log(updatedHtmlStrings, updatedHtmlStrings)
    const angularTemplate = compiler.parseTemplate(updatedHtmlStrings.join(''), id)
    const { inputs, outputs } = await findInputAndOutputAssignments(sourceFile)
    const { viewQueries, contentQueries, constantDeclarations } = await findViewChildAndContentQueries(sourceFile)

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
            defer: {
                dependenciesFn: null,
                mode: 1,
              },
            deps: [],
            animations: null,
            i18nUseExternalIds: false,
            interpolation: compiler.DEFAULT_INTERPOLATION_CONFIG,
            isSignal: true,
            providers: null,
            queries: [...contentQueries, ...viewQueries],
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
            usesInheritance: false,
            viewProviders: null,
            viewQueries: viewQueries,
        },
        constantPool,
        compiler.makeBindingParser(compiler.DEFAULT_INTERPOLATION_CONFIG)
    );

    (out.expression as any).args[0].entries.push(new LiteralMapEntry('dependencies', new compiler.LiteralArrayExpr(
        addToDeclatoration.map(val => new compiler.WrappedNodeExpr(val))) as any, false))

    const printerIntance = new printer.Printer();
    let strExpression = out.expression.visitExpression(
        printerIntance,
        new printer.Context(false)
    );
    

      for (const stmt of constantPool.statements) {
        const strStmt = stmt.visitStatement(printerIntance, new printer.Context(false));
    
        strExpression += `\n\n${strStmt}`;
      }

    // const treatyIvy = createWrapper(toCamelCase(fileName), sourceFile, strExpression, constantDeclarations.join('\n'))
    const treatyIvy = createWrapper(toCamelCase(fileName), sourceFile, strExpression, '')
    return treatyIvy;
}