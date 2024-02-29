import { parseAsync } from 'oxc-parser';


const scf = ` 
---
import MyComponent from "../components/MyComponent.astro"

const foregroundColor = "rgb(221 243 228)";
const backgroundColor = "rgb(24 121 78)";

const someData = "Hello World";
function doSomething() {
  const a = 'abc';
  console.log(someData);
  console.log(a);
}

doSomething();
---
<style define:vars={{ foregroundColor, backgroundColor }}>
  h1 {
    background-color: var(--backgroundColor);
    color: var(--foregroundColor);
  }
</style>
<h1>{{someData}}</h1>
<MyComponent class="red">This will be red!</MyComponent>

<button (onClick)="doSomething()">Do something</button>
`;


function adjustIdentifierReferences(node, propertyNames, methodNames) {
  if (!node) return;

  switch (node.type) {
    case 'IdentifierReference':
      if (propertyNames.has(node.name)) {
        return `this.${node.name}`;
      } else if (methodNames.has(node.name)) {
        return `this.${node.name}()`;
      } else {
        return node.name;
      }

    case 'CallExpression':
      const callee = adjustIdentifierReferences(node.callee, propertyNames, methodNames);
      const args = node.arguments.map(arg => adjustIdentifierReferences(arg, propertyNames, methodNames)).join(', ');
      return `${callee}(${args})`;
    case 'ExpressionStatement':
      if (node.expression.type === 'CallExpression') {
        const args = node.expression.arguments.map(a => {
          return a;
        });

        const calleeName = node.expression.callee.name;
        if (methodNames.has(calleeName)) {
          `this.${calleeName}();`
        } else {
          return `${calleeName}();`
        }
      }
      break;
    case 'VariableDeclaration':
      const prep =  node.declarations.map(declaration => {
        const varName = declaration.id.kind.name;
        return `${node.kind} ${varName} = ${JSON.stringify(declaration.init.value)}`;
      }).join('\n');
      break;
    case 'FunctionDeclaration':
      const funcName = node.id.name;
      methodNames.add(funcName);
      let funcBody = processFunctionBody(node.body, propertyNames, methodNames);
      return `${funcName}() {\n  ${funcBody}  }`
  }
  return '';
}

function processFunctionBody(body, propertyNames, methodNames) {
  let processedBody = '';

  body.statements.forEach(statement => {
    if (statement.type === 'FunctionDeclaration') {
      const funcName = node.id.name;
      methodNames = [...methodNames].filter(name => name !== funcName);
    } else if (statement.type === 'VariableDeclaration') {
      const varNames = statement.declarations.map(d => d.id.kind.name);
      propertyNames = [...propertyNames].filter(name => !varNames.includes(name));
    } 
    processedBody += adjustIdentifierReferences(statement, propertyNames, methodNames) + '\n';
  });

  return processedBody;
}

function ASTHandler(ast) {
  const imports = [];
  const properties = new Set();
  const methods = [];
  const constructorCalls = [];

  const propertyNames = new Set();
  const methodNames = new Set();

  ast.body.forEach(node => {
    switch (node.type) {
      case 'ImportDeclaration':
        imports.push(`import ${node.specifiers.map(s => s.local.name).join(', ')} from '${node.source.value.replace('.astro', '')}';`);
        break;
      case 'VariableDeclaration':
        node.declarations.forEach(declaration => {
          const varName = declaration.id.kind.name;
          propertyNames.add(varName);
          properties.add(`${varName}: any = ${JSON.stringify(declaration.init.value)};`);
        });
        break;
      case 'FunctionDeclaration':
        const funcName = node.id.name;
        methodNames.add(funcName);
        let funcBody = processFunctionBody(node.body, propertyNames, methodNames);
        methods.push(`  ${funcName}() {\n  ${funcBody}  }`);
        break;
      case 'ExpressionStatement':
        if (node.expression.type === 'CallExpression') {
          const calleeName = node.expression.callee.name;
          if (methodNames.has(calleeName)) {
            constructorCalls.push(`this.${calleeName}();`);
          } else {
            constructorCalls.push(`${calleeName}();`);
          }
        }
        break;
    }
  });

  return { imports: [...imports], properties: [...properties], methods, constructorCalls };
}

async function parseAstroSFCToAngular(content) {
  const scriptRegex = /---\n?([\s\S]*?)\n?---/;
  const styleRegex = /<style[^>]*>((?:.|\n|\r\n)*)<\/style>/;

  const scriptMatch = content.match(scriptRegex);
  const styleMatch = content.match(styleRegex);

  const scriptSection = scriptMatch ? scriptMatch[1].trim() : '';
  const stylesSection = styleMatch ? styleMatch[1].trim() : '';

  const AST = await parseAsync(scriptSection, { sourceType: 'unambiguous' });

  const { constructorCalls, imports, methods, properties } = ASTHandler(JSON.parse(AST.program));

  let constructorMethod = constructorCalls.length > 0 ? `constructor() {\n    ${constructorCalls.join('\n    ')}\n  }\n` : '';

  const angularComponent = `
${imports.join('\n')}
@Component({
  selector: 'app-custom-component',
  template: \`
    ${content.replace(scriptRegex, '').replace(styleRegex, '').trim()}
  \`,
  styles: [\`${stylesSection}\`]
})
export class CustomComponent {
  ${properties.join('\n  ')}
  ${constructorMethod}
  ${methods.join('\n  ')}
}
`;

  return angularComponent;
}



const angularComponent = await parseAstroSFCToAngular(scf);
console.log(angularComponent);


debugger;


