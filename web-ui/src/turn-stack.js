export function turnUsageHost(userNodes, expanded) {
  return expanded ? userNodes[0] : userNodes.at(-1);
}

export function orderedTurnChildren(messageNodes, userNodes, foldBlock, stack, expanded) {
  const firstPrompt = userNodes[0] || messageNodes[0];
  if (!expanded && userNodes.length > 1) {
    const userSet = new Set(userNodes);
    return messageNodes.flatMap((node) => {
      if (node === firstPrompt) return [stack, foldBlock];
      return userSet.has(node) ? [] : [node];
    });
  }
  const orderedNodes = [...messageNodes];
  orderedNodes.splice(orderedNodes.indexOf(firstPrompt) + 1, 0, foldBlock);
  return orderedNodes;
}
