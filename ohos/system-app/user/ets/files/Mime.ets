import uniformTypeDescriptor from '@ohos.data.uniformTypeDescriptor';
import { extension } from './Docs';

// The MIME type of a file name's extension, or '' if OpenHarmony knows none.
export function mimeOf(name: string): string {
  const ext = extension(name);
  if (ext === '') {
    return '';
  }
  try {
    const id = uniformTypeDescriptor.getUniformDataTypeByFilenameExtension(ext);
    const types = uniformTypeDescriptor.getTypeDescriptor(id).mimeTypes;
    return types.length > 0 ? types[0] : '';
  } catch (e) {
    return '';
  }
}
