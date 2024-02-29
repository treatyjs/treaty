import { parseAsync } from 'oxc-parser';


const scf = ` 
import MyComponent from "../components/MyComponent.ngx"

const foregroundColor = "rgb(221 243 228)";
const backgroundColor = "rgb(24 121 78)";

doSomething();
<style>
  h1 {
    background-color: var(--backgroundColor);
    color: var(--foregroundColor);
  }
</style>
const someData = "Hello World";
<h1>{{someData}}</h1>
<MyComponent class="red">This will be red!</MyComponent>


function doSomething() {
  const a = 'abc';
  console.log(someData);
  console.log(a);
}
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
        // const args = node.expression.arguments.map(a => )

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
        const specifiersNames = node.specifiers.map(s => s.local.name)
        imports.push({ specifiers: specifiersNames,  content: `import ${specifiersNames.join(', ')} from '${node.source.value.replace('.ngx', '')}';`});
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
  const cssContent = [];
  const htmlContent = [];

  const cssRegex = /<style(?:\s+lang="(css|scss)")?[^>]*>([\s\S]*?)<\/style>/g;
  const angularHtmlRegex = /<([A-Za-z0-9\-_]+)(\s+[^>]*?(\{\{.*?\}\}|[\[\(]\(?.+?\)?[\]\)]|[\*\#][A-Za-z0-9\-_]+=".*?"))*[\s\S]*?<\/\1>/g;

  const jsTsContent = content.replace(cssRegex, (match, lang, css) => {
    cssContent.push({lang: lang || 'css', content: css});
    return '';
}).replace(angularHtmlRegex, (match) => {
    htmlContent.push(match);
    return '';
});

  const AST = await parseAsync(jsTsContent, { sourceType: 'unambiguous' });

  const { constructorCalls, imports, methods, properties } = ASTHandler(JSON.parse(AST.program));

  let constructorMethod = constructorCalls.length > 0 ? `constructor() {\n    ${constructorCalls.join('\n    ')}\n  }\n` : '';

  function findImportsInHTML(imports, html) {
    return imports.reduce((acc, { specifiers }) => {
      specifiers.forEach(specifierName => {
        const tagName = specifierName.replace(/([A-Z])/g, '-$1').toLowerCase();
        const tagRegex = new RegExp(`<(${specifierName}|${tagName})\\s`, 'g');
        if (tagRegex.test(html)) acc.push(specifierName);
      });
      return acc;
    }, []);
  }
  
  const html = htmlContent.join('\n');
  const angularComponent = `
${imports.map(i => i.content).join('\n')}
@Component({
  selector: 'app-custom-component',
  import: [${findImportsInHTML(imports, html).join(',')}],
  template: \`
    ${html}
  \`,
  styles: [${cssContent.map(item => `\`${item.content}\``).join(',')}]
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


