"""Portable regression tests for PR title and commit subject syntax."""
from contextlib import redirect_stderr
import io
import unittest

from check_contribution import check_subject


class ContributionSubjectTests(unittest.TestCase):
    def test_accepts_varied_types_and_scopes(self):
        for subject in (
            'fix(ci): validate SDK versions',
            'docs(contributing): clarify commit format',
            'feat(cli): add an option',
            'refactor(obfuscator): simplify pass selection',
            'test(string-encryption): cover empty inputs',
            'build(deps): update dependencies',
            'chore(github-actions): update actions',
            'perf(src/main.rs): reduce allocations',
            'revert(wrapper): restore argument handling',
            'fix(ci): x',
            'docs(readme): explain café example',
        ):
            for label in ('PR title', 'commit abcdef123456'):
                with self.subTest(subject=subject, label=label):
                    errors = io.StringIO()
                    with redirect_stderr(errors):
                        self.assertTrue(check_subject(subject, label))
                    self.assertEqual(errors.getvalue(), '')

    def test_rejects_malformed_subjects(self):
        for subject in (
            None, 123, [], {}, '',
            'fix: missing scope',
            '(ci): missing type',
            'fix(): empty scope',
            'Fix(ci): uppercase type',
            'fix2(ci): invalid type',
            'fix (ci): space before scope',
            'fix(build ci): whitespace in scope',
            'fix(build\tci): tab in scope',
            'fix(build\nci): newline in scope',
            'fix((ci)): nested scope',
            'fix(ci)): extra closing parenthesis',
            'fix(ci):',
            'fix(ci): ',
            'fix(ci):missing separator space',
            'fix(ci):  extra leading space',
            'fix(ci):\tdescription',
            ' fix(ci): leading whitespace',
            'fix(ci): trailing whitespace ',
            'fix(ci): trailing tab\t',
            'fix(ci): first\nsecond',
            'fix(ci): first\rsecond',
            'fix(ci): trailing newline\n',
            'fix(ci)!: unsupported separator',
        ):
            for label in ('PR title', 'commit abcdef123456'):
                with self.subTest(subject=subject, label=label):
                    errors = io.StringIO()
                    with redirect_stderr(errors):
                        self.assertFalse(check_subject(subject, label))
                    self.assertEqual(
                        errors.getvalue(),
                        f'error: {label} must use type(scope): <short description>\n',
                    )


if __name__ == '__main__':
    unittest.main()
