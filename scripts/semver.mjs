// Shared SemVer 2.0.0 validation for the packaging scripts.
//
// The pattern follows the official SemVer 2.0.0 grammar:
// <valid semver> ::= <version core> | <version core> "-" <pre-release> | <version core> "+" <build> | <version core> "-" <pre-release> "+" <build>
// <version core> ::= <major> "." <minor> "." <patch>
const semverPattern =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$/;

// Report whether the value is a valid SemVer 2.0.0 version.
export function isSemver(value) {
  return typeof value === 'string' && semverPattern.test(value) && !/[\r\n]/.test(value);
}

export { semverPattern };
