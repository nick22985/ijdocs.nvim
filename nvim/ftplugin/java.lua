-- clean this up let users configure this
local ok, ij = pcall(require, "ijdocs")
if not ok then
	return
end

local function set_keymaps(bufnr)
	local opts = { buffer = bufnr, silent = true }
	vim.keymap.set(
		"n",
		"K",
		ij.hover,
		vim.tbl_extend("force", opts, {
			desc = "ijdocs: rich javadoc hover",
		})
	)
	vim.keymap.set(
		"n",
		"gK",
		ij.open_browser,
		vim.tbl_extend("force", opts, {
			desc = "ijdocs: open javadoc in browser",
		})
	)
end

local bufnr = vim.api.nvim_get_current_buf()
set_keymaps(bufnr)

vim.api.nvim_create_autocmd("LspAttach", {
	buffer = bufnr,
	callback = function()
		vim.schedule(function()
			vim.defer_fn(function()
				if vim.api.nvim_buf_is_valid(bufnr) then
					set_keymaps(bufnr)
				end
			end, 50)
		end)
	end,
})
