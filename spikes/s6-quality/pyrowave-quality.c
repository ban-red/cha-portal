// pyrowave-quality: a y4m clip through PyroWave the way the streamer sends
// it. Each frame is encoded within a byte budget and cut into 1100-byte
// packets (as cha-streamer does), then decoded from those packets. The output
// y4m is what a viewer would see when nothing is lost.
//
//   pyrowave-quality <in.y4m> <out.y4m> <bytes_per_frame>
//
// 4:2:0 or 4:4:4 follows the input (`C444` in the header means 4:4:4).
// Prints the mean bytes per frame actually used.

#include <vulkan/vulkan.h>
#include "pyrowave.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define PACKET_BOUNDARY 1100

static void check(const char *what, pyrowave_result r)
{
	if (r != PYROWAVE_SUCCESS)
	{
		fprintf(stderr, "%s failed (%d)\n", what, r);
		exit(1);
	}
}

static void *alloc(size_t size)
{
	void *p = malloc(size);
	if (!p)
	{
		fprintf(stderr, "out of memory\n");
		exit(1);
	}
	return p;
}

int main(int argc, char **argv)
{
	if (argc != 4)
	{
		fprintf(stderr, "usage: pyrowave-quality <in.y4m> <out.y4m> <bytes_per_frame>\n");
		return 2;
	}
	FILE *in = fopen(argv[1], "rb");
	FILE *out = fopen(argv[2], "wb");
	if (!in || !out)
	{
		perror("opening the clips");
		return 1;
	}
	size_t budget = strtoul(argv[3], NULL, 0);

	char header[512], tokens[512], line[64];
	if (!fgets(header, sizeof header, in) || strncmp(header, "YUV4MPEG2 ", 10) != 0)
	{
		fprintf(stderr, "not a y4m file\n");
		return 1;
	}
	int width = 0, height = 0;
	strcpy(tokens, header);
	for (char *t = strtok(tokens, " \n"); t; t = strtok(NULL, " \n"))
	{
		if (t[0] == 'W')
			width = atoi(t + 1);
		else if (t[0] == 'H')
			height = atoi(t + 1);
	}
	int is444 = strstr(header, " C444") != NULL;
	fputs(header, out);

	pyrowave_device device;
	check("pyrowave_create_device_by_compat2",
	      pyrowave_create_device_by_compat2(0, 0, NULL, NULL, NULL, VK_QUEUE_GLOBAL_PRIORITY_MEDIUM, &device));
	pyrowave_chroma_subsampling chroma = is444 ? PYROWAVE_CHROMA_SUBSAMPLING_444 : PYROWAVE_CHROMA_SUBSAMPLING_420;

	pyrowave_encoder_create_info enc_info = { device, width, height, chroma };
	pyrowave_encoder encoder;
	check("pyrowave_encoder_create", pyrowave_encoder_create(&enc_info, &encoder));
	pyrowave_decoder_create_info dec_info = { device, width, height, chroma, false };
	pyrowave_decoder decoder;
	check("pyrowave_decoder_create", pyrowave_decoder_create(&dec_info, &decoder));

	int cw = is444 ? width : width / 2, ch = is444 ? height : height / 2;
	size_t sizes[3] = { (size_t)width * height, (size_t)cw * ch, (size_t)cw * ch };
	pyrowave_cpu_buffer src = { 0 }, dst = { 0 };
	for (int i = 0; i < 3; i++)
	{
		src.data[i] = alloc(sizes[i]);
		dst.data[i] = alloc(sizes[i]);
		src.row_stride_in_bytes[i] = dst.row_stride_in_bytes[i] = i ? cw : width;
		src.plane_size_in_bytes[i] = dst.plane_size_in_bytes[i] = sizes[i];
	}
	src.width = dst.width = width;
	src.height = dst.height = height;
	src.format = dst.format = is444 ? PYROWAVE_CPU_BUFFER_FORMAT_YUV444P : PYROWAVE_CPU_BUFFER_FORMAT_YUV420P;

	pyrowave_rate_control rate = { budget };
	size_t bits_size = budget + (1 << 20);
	unsigned char *bits = alloc(bits_size);
	size_t max_packets = 0, frames = 0, total_bytes = 0, partial = 0;
	pyrowave_packet *packets = NULL;

	while (fgets(line, sizeof line, in))
	{
		if (strncmp(line, "FRAME", 5) != 0)
		{
			fprintf(stderr, "bad frame header\n");
			return 1;
		}
		for (int i = 0; i < 3; i++)
			if (fread(src.data[i], 1, sizes[i], in) != sizes[i])
			{
				fprintf(stderr, "short frame\n");
				return 1;
			}

		check("encoding", pyrowave_encoder_encode_cpu_synchronous(encoder, &src, &rate));
		size_t count = 0, written = 0;
		check("counting packets", pyrowave_encoder_compute_num_packets(encoder, PACKET_BOUNDARY, &count));
		if (count > max_packets)
		{
			free(packets);
			packets = alloc(count * sizeof *packets);
			max_packets = count;
		}
		check("packetizing",
		      pyrowave_encoder_packetize(encoder, packets, PACKET_BOUNDARY, &written, bits, bits_size));

		pyrowave_decoder_clear(decoder);
		for (size_t i = 0; i < written; i++)
		{
			check("pushing a packet", pyrowave_decoder_push_packet(decoder, bits + packets[i].offset, packets[i].size));
			total_bytes += packets[i].size;
		}
		if (!pyrowave_decoder_decode_is_ready(decoder, false))
			partial++;
		check("decoding", pyrowave_decoder_decode_cpu_buffer_synchronous(decoder, &dst));

		fputs("FRAME\n", out);
		for (int i = 0; i < 3; i++)
			fwrite(dst.data[i], 1, sizes[i], out);
		frames++;
	}

	printf("%zu frames, %zu bytes per frame on average (budget %zu)%s\n", frames,
	       frames ? total_bytes / frames : 0, budget, partial ? ", some frames incomplete" : "");
	pyrowave_decoder_destroy(decoder);
	pyrowave_encoder_destroy(encoder);
	pyrowave_device_destroy(device);
	fclose(out);
	return partial ? 1 : 0;
}
