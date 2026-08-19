const path = require('node:path');
const Mocha = require('mocha');

function run() {
  return new Promise((resolve, reject) => {
    const mocha = new Mocha({ ui: 'tdd', color: true, timeout: 30_000 });
    mocha.addFile(path.resolve(__dirname, 'extension.test.js'));
    mocha.run((failures) => {
      if (failures > 0) {
        reject(new Error(`${failures} VS Code extension test(s) failed.`));
      } else {
        resolve();
      }
    });
  });
}

module.exports = { run };
