import re

def resolve_conflict_file(filepath):
    with open(filepath, 'r', encoding='utf-8', errors='ignore') as f:
        content = f.read()

    if '<<<<<<<' not in content:
        return False

    pattern = re.compile(r'<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n', re.DOTALL)

    def replacer(match):
        ours = match.group(1)
        theirs = match.group(2)
        # If theirs contains something completely absent in ours, or additions, merge both or smartly combine
        # Common pattern: imports, functions, test cases
        ours_lines = ours.splitlines(keepends=True)
        theirs_lines = theirs.splitlines(keepends=True)
        
        # Check if theirs is just adding tests/functions to the end
        merged = []
        for line in ours_lines:
            merged.append(line)
        for line in theirs_lines:
            if line not in ours_lines:
                merged.append(line)
        return "".join(merged)

    new_content = pattern.sub(replacer, content)
    with open(filepath, 'w', encoding='utf-8') as f:
        f.write(new_content)
    return True

