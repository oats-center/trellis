export function subjectMatches(filter: string, subject: string): boolean {
  const filterTokens = filter.split(".");
  const subjectTokens = subject.split(".");
  for (let index = 0; index < filterTokens.length; index += 1) {
    const token = filterTokens[index];
    if (token === ">") {
      return index === filterTokens.length - 1 && index < subjectTokens.length;
    }
    if (
      index >= subjectTokens.length ||
      (token !== "*" && token !== subjectTokens[index])
    ) return false;
  }
  return filterTokens.length === subjectTokens.length;
}
